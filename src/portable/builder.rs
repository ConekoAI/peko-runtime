//! Agent builder — construct `.agent` packages from a source directory
//!
//! This module provides `AgentBuilder`, which reads an agent directory
//! (containing `config/agent.toml`), categorizes files into content-addressable
//! layers, stores them in a local `AgentRegistry`, and produces a tagged
//! `.agent` package.
//!
//! ## Layer semantics
//!
//! | Layer     | Source directory | Required | Behaviour config? |
//! |-----------|------------------|----------|-------------------|
//! | `config`  | `config/`        | Yes      | Yes (agent.toml)  |
//! | `identity`| `identity/`      | Yes      | No                |
//! | `skills`  | `skills/`        | No       | No                |
//! | `workspace`| `workspace/`    | No       | No                |
//! | `sessions`| `sessions/`      | No       | No                |
//! | `mcp`     | `mcp/`           | No       | No                |
//!
//! ## Usage
//!
//! ```rust,ignore
//! let registry = AgentRegistry::new(AgentRegistry::default_path());
//! registry.init().await?;
//!
//! let result = AgentBuilder::build_from_directory(
//!     Path::new("./my-agent"),
//!     "my-agent:v1.0",
//!     &registry,
//!     |progress| println!("{:?}", progress),
//! ).await?;
//! ```

use crate::portable::manifest::{AgentLayers, AgentManifest};
use crate::portable::registry::AgentRegistry;
use crate::portable::types::{compute_digest, LayerType};
use anyhow::Context;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Build progress events
#[derive(Debug, Clone)]
pub enum BuildProgress {
    /// Started reading source directory
    Reading { path: PathBuf },
    /// Processing a layer
    Layering {
        layer_type: LayerType,
        digest: String,
        size_bytes: u64,
    },
    /// Build completed
    Complete {
        package_path: PathBuf,
        tag: String,
        manifest_digest: String,
        layer_count: usize,
        total_size_bytes: u64,
    },
}

/// Result of a successful build
#[derive(Debug, Clone)]
pub struct BuildResult {
    /// Path to the created `.agent` file
    pub package_path: PathBuf,
    /// Tag assigned to the manifest
    pub tag: String,
    /// Manifest digest (sha256:...)
    pub manifest_digest: String,
    /// Number of layers created
    pub layer_count: usize,
    /// Total size of all layers in bytes
    pub total_size_bytes: u64,
    /// The manifest that was built
    pub manifest: AgentManifest,
}

/// Builder for creating `.agent` packages from a source directory
pub struct AgentBuilder;

impl AgentBuilder {
    /// Build a `.agent` package from a directory.
    ///
    /// # Arguments
    ///
    /// * `source_path` — Directory containing `config/agent.toml`
    /// * `tag` — Tag in `name:version` format (e.g. `my-agent:v1.0`)
    /// * `registry` — Local `AgentRegistry` for layer/manifest storage
    /// * `progress` — Callback for build progress events
    ///
    /// # Errors
    ///
    /// Returns an error if the source directory is invalid, required files are
    /// missing, or I/O fails.
    pub async fn build_from_directory<F>(
        source_path: &Path,
        tag: &str,
        registry: &AgentRegistry,
        mut progress: F,
    ) -> anyhow::Result<BuildResult>
    where
        F: FnMut(BuildProgress),
    {
        // ── Validate source ──────────────────────────────────────────────
        if !source_path.exists() {
            anyhow::bail!("Source path does not exist: {}", source_path.display());
        }
        if !source_path.is_dir() {
            anyhow::bail!("Source path is not a directory: {}", source_path.display());
        }

        let config_toml_path = source_path.join("config").join("agent.toml");
        if !config_toml_path.exists() {
            anyhow::bail!(
                "Source directory does not contain config/agent.toml: {}",
                source_path.display()
            );
        }

        progress(BuildProgress::Reading {
            path: source_path.to_path_buf(),
        });

        // ── Read agent.toml to extract metadata ──────────────────────────
        let config_toml = tokio::fs::read_to_string(&config_toml_path)
            .await
            .with_context(|| format!("Failed to read {}", config_toml_path.display()))?;

        let agent_config: crate::types::agent::AgentConfig = toml::from_str(&config_toml)
            .with_context(|| format!("Failed to parse agent.toml in {}", source_path.display()))?;

        let agent_name = agent_config.name.clone();
        let description = agent_config.description.clone();

        // ── Build layers ─────────────────────────────────────────────────
        let mut layers = AgentLayers::default();
        let mut total_size: u64 = 0;
        let mut layer_count: usize = 0;

        // Config layer (required)
        let (config_digest, config_size) =
            Self::build_layer(source_path, LayerType::Config, registry, &mut progress).await?;
        layers.config = Some(config_digest.clone());
        total_size += config_size;
        layer_count += 1;

        // Identity layer (required)
        let (identity_digest, identity_size) =
            Self::build_layer(source_path, LayerType::Identity, registry, &mut progress).await?;
        layers.identity = Some(identity_digest.clone());
        total_size += identity_size;
        layer_count += 1;

        // Optional layers
        for layer_type in [
            LayerType::Skills,
            LayerType::Workspace,
            LayerType::Sessions,
            LayerType::Mcp,
        ] {
            if let Some((digest, size)) =
                Self::build_optional_layer(source_path, layer_type, registry, &mut progress).await?
            {
                match layer_type {
                    LayerType::Skills => layers.skills = Some(digest.clone()),
                    LayerType::Workspace => layers.workspace = Some(digest.clone()),
                    LayerType::Sessions => layers.sessions = Some(digest.clone()),
                    LayerType::Mcp => layers.mcp = Some(digest.clone()),
                    _ => unreachable!(),
                }
                total_size += size;
                layer_count += 1;
            }
        }

        // ── Create manifest ──────────────────────────────────────────────
        let mut manifest = AgentManifest::new(
            &agent_name,
            "1.0.0", // Package version (not agent version)
            &format!("did:pekobot:local:{}", agent_name),
        );
        manifest.agent.description = description;
        manifest.layers = Some(layers);

        // ── Build .agent archive ─────────────────────────────────────────
        let package_path = Self::create_agent_archive(source_path, &manifest, registry).await?;

        // ── Store manifest and tag ───────────────────────────────────────
        let manifest_digest = registry
            .store_manifest(&manifest, Some(tag))
            .await
            .context("Failed to store manifest in registry")?;

        progress(BuildProgress::Complete {
            package_path: package_path.clone(),
            tag: tag.to_string(),
            manifest_digest: manifest_digest.as_str().to_string(),
            layer_count,
            total_size_bytes: total_size,
        });

        Ok(BuildResult {
            package_path,
            tag: tag.to_string(),
            manifest_digest: manifest_digest.as_str().to_string(),
            layer_count,
            total_size_bytes: total_size,
            manifest,
        })
    }

    // ------------------------------------------------------------------
    // Layer building
    // ------------------------------------------------------------------

    /// Build a required layer.
    ///
    /// # Errors
    ///
    /// Returns an error if the layer directory does not exist or is empty.
    async fn build_layer<F>(
        source_path: &Path,
        layer_type: LayerType,
        registry: &AgentRegistry,
        progress: &mut F,
    ) -> anyhow::Result<(String, u64)>
    where
        F: FnMut(BuildProgress),
    {
        let dir_name = layer_type.dir_name();
        let layer_dir = source_path.join(dir_name);

        if !layer_dir.exists() {
            anyhow::bail!("Required layer directory missing: {}/", dir_name);
        }

        let (digest, size) = Self::pack_layer(&layer_dir, layer_type, registry).await?;

        progress(BuildProgress::Layering {
            layer_type,
            digest: digest.clone(),
            size_bytes: size,
        });

        Ok((digest, size))
    }

    /// Build an optional layer — returns `None` if the directory is missing or empty.
    async fn build_optional_layer<F>(
        source_path: &Path,
        layer_type: LayerType,
        registry: &AgentRegistry,
        progress: &mut F,
    ) -> anyhow::Result<Option<(String, u64)>>
    where
        F: FnMut(BuildProgress),
    {
        let dir_name = layer_type.dir_name();
        let layer_dir = source_path.join(dir_name);

        if !layer_dir.exists() {
            return Ok(None);
        }

        // Check if directory has any files
        let has_files = Self::dir_has_files(&layer_dir).await?;
        if !has_files {
            return Ok(None);
        }

        let (digest, size) = Self::pack_layer(&layer_dir, layer_type, registry).await?;

        progress(BuildProgress::Layering {
            layer_type,
            digest: digest.clone(),
            size_bytes: size,
        });

        Ok(Some((digest, size)))
    }

    /// Recursively check if a directory contains any files.
    async fn dir_has_files(dir: &Path) -> anyhow::Result<bool> {
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                return Ok(true);
            }
            if path.is_dir() && Box::pin(Self::dir_has_files(&path)).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Pack a layer directory into a tarball, compute its digest, and store in registry.
    ///
    /// Returns `(digest, size_bytes)`.
    async fn pack_layer(
        layer_dir: &Path,
        layer_type: LayerType,
        registry: &AgentRegistry,
    ) -> anyhow::Result<(String, u64)> {
        // Collect all files in deterministic order
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        Self::collect_files_recursive(layer_dir, "", &mut files).await?;

        if files.is_empty() {
            anyhow::bail!("Layer directory is empty: {}", layer_dir.display());
        }

        // Build tarball in memory
        let tar_bytes = Self::build_tarball(&files)?;
        let size = tar_bytes.len() as u64;

        // Compute digest
        let digest = compute_digest(&tar_bytes);

        // Store in registry (idempotent — skipped if already present)
        registry.store_layer(&digest, &tar_bytes).await?;

        let _ = layer_type; // used in future for typed storage
        Ok((digest, size))
    }

    /// Recursively collect files from a directory into a sorted map.
    ///
    /// `prefix` is the relative path inside the layer (e.g. `""` or `"sub/dir/"`).
    async fn collect_files_recursive(
        dir: &Path,
        prefix: &str,
        files: &mut BTreeMap<String, Vec<u8>>,
    ) -> anyhow::Result<()> {
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let rel_path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}{name}")
            };

            if path.is_dir() {
                Box::pin(Self::collect_files_recursive(
                    &path,
                    &format!("{rel_path}/"),
                    files,
                ))
                .await?;
            } else {
                let content = tokio::fs::read(&path).await?;
                files.insert(rel_path, content);
            }
        }
        Ok(())
    }

    /// Build a tarball from a map of `(relative_path, bytes)`.
    fn build_tarball(files: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<Vec<u8>> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);

            for (path, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_path(path)?;
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append(&header, content.as_slice())?;
            }

            tar.finish()?;
        }
        Ok(buf)
    }

    // ------------------------------------------------------------------
    // Archive creation
    // ------------------------------------------------------------------

    /// Create the final `.agent` archive.
    ///
    /// The archive contains:
    /// - `manifest.toml`
    /// - Each layer extracted from the registry and placed at `{layer_type}/...`
    async fn create_agent_archive(
        source_path: &Path,
        manifest: &AgentManifest,
        _registry: &AgentRegistry,
    ) -> anyhow::Result<PathBuf> {
        let agent_name = &manifest.agent.name;
        let package_path = PathBuf::from(format!("{agent_name}.agent"));

        let tar_gz = std::fs::File::create(&package_path)?;
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Add manifest.toml
        let manifest_toml = manifest.to_toml()?;
        let manifest_bytes = manifest_toml.into_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_path("manifest.toml")?;
        header.set_size(manifest_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, manifest_bytes.as_slice())?;

        // Add each layer's files directly (flattened structure for the .agent file)
        if let Some(layers) = &manifest.layers {
            Self::append_layer_files(
                &mut tar,
                source_path,
                LayerType::Config,
                layers.config.as_deref(),
            )?;
            Self::append_layer_files(
                &mut tar,
                source_path,
                LayerType::Identity,
                layers.identity.as_deref(),
            )?;
            Self::append_layer_files(
                &mut tar,
                source_path,
                LayerType::Skills,
                layers.skills.as_deref(),
            )?;
            Self::append_layer_files(
                &mut tar,
                source_path,
                LayerType::Workspace,
                layers.workspace.as_deref(),
            )?;
            Self::append_layer_files(
                &mut tar,
                source_path,
                LayerType::Sessions,
                layers.sessions.as_deref(),
            )?;
            Self::append_layer_files(&mut tar, source_path, LayerType::Mcp, layers.mcp.as_deref())?;
        }

        tar.finish()?;
        Ok(package_path)
    }

    /// Append files from a source directory into the tar archive at the correct layer prefix.
    fn append_layer_files(
        tar: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
        source_path: &Path,
        layer_type: LayerType,
        _digest: Option<&str>,
    ) -> anyhow::Result<()> {
        let dir_name = layer_type.dir_name();
        let layer_dir = source_path.join(dir_name);

        if !layer_dir.exists() {
            return Ok(());
        }

        Self::append_dir_recursive(tar, &layer_dir, dir_name)?;
        Ok(())
    }

    /// Recursively append a directory to the tar archive.
    fn append_dir_recursive(
        tar: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
        src_dir: &Path,
        tar_prefix: &str,
    ) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(src_dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let tar_path = format!("{tar_prefix}/{name}");

            if path.is_dir() {
                Self::append_dir_recursive(tar, &path, &tar_path)?;
            } else {
                let content = std::fs::read(&path)?;
                let mut header = tar::Header::new_gnu();
                header.set_path(&tar_path)?;
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append(&header, content.as_slice())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portable::registry::AgentRegistry;

    /// Create a minimal agent directory for testing
    async fn create_test_agent_dir(base: &Path) -> anyhow::Result<()> {
        // config/agent.toml
        let config_dir = base.join("config");
        tokio::fs::create_dir_all(&config_dir).await?;
        let agent_toml = r#"
name = "test-agent"
description = "A test agent"
auto_accept_trusted = false
default_timeout_seconds = 300

[provider]
provider_type = "open_a_i"
api_key = "sk-test"
default_model = "gpt-4"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.gpt-4]
name = "gpt-4"
max_tokens = 4096
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0
"#;
        tokio::fs::write(config_dir.join("agent.toml"), agent_toml).await?;

        // identity/did.json
        let identity_dir = base.join("identity");
        tokio::fs::create_dir_all(&identity_dir).await?;
        tokio::fs::write(
            identity_dir.join("did.json"),
            r#"{"id":"did:pekobot:test"}"#,
        )
        .await?;

        // skills/test-skill/SKILL.md
        let skills_dir = base.join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skills_dir).await?;
        tokio::fs::write(skills_dir.join("SKILL.md"), "# Test Skill").await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_build_from_directory_minimal() {
        let temp_dir = tempfile::tempdir().unwrap();
        let agent_dir = temp_dir.path().join("test-agent");
        create_test_agent_dir(&agent_dir).await.unwrap();

        let registry_dir = temp_dir.path().join("registry");
        let registry = AgentRegistry::new(&registry_dir);
        registry.init().await.unwrap();

        let mut progress_events = Vec::new();
        let result =
            AgentBuilder::build_from_directory(&agent_dir, "test-agent:v1.0", &registry, |p| {
                progress_events.push(p)
            })
            .await
            .unwrap();

        assert_eq!(result.tag, "test-agent:v1.0");
        assert_eq!(result.layer_count, 3); // config, identity, skills
        assert!(result.total_size_bytes > 0);
        assert!(result.package_path.exists());

        // Verify manifest stored in registry
        let manifest = registry
            .get_manifest_by_tag("test-agent:v1.0")
            .await
            .unwrap();
        assert_eq!(manifest.agent.name, "test-agent");
        assert!(manifest.layers.is_some());

        let layers = manifest.layers.unwrap();
        assert!(layers.config.is_some());
        assert!(layers.identity.is_some());
        assert!(layers.skills.is_some());
        assert!(layers.workspace.is_none());
        assert!(layers.sessions.is_none());
        assert!(layers.mcp.is_none());

        // Verify layers stored in registry
        assert!(registry.has_layer(layers.config.as_ref().unwrap()));
        assert!(registry.has_layer(layers.identity.as_ref().unwrap()));
        assert!(registry.has_layer(layers.skills.as_ref().unwrap()));

        // Verify progress events
        assert!(!progress_events.is_empty());
        let has_complete = progress_events
            .iter()
            .any(|p| matches!(p, BuildProgress::Complete { .. }));
        assert!(has_complete);
    }

    #[tokio::test]
    async fn test_build_missing_config_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let agent_dir = temp_dir.path().join("bad-agent");
        tokio::fs::create_dir_all(&agent_dir).await.unwrap();
        // No config/agent.toml

        let registry = AgentRegistry::new(temp_dir.path().join("registry"));
        registry.init().await.unwrap();

        let result =
            AgentBuilder::build_from_directory(&agent_dir, "bad:v1", &registry, |_| {}).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("config/agent.toml"));
    }

    #[tokio::test]
    async fn test_build_missing_identity_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let agent_dir = temp_dir.path().join("agent");
        tokio::fs::create_dir_all(agent_dir.join("config"))
            .await
            .unwrap();
        tokio::fs::write(
            agent_dir.join("config").join("agent.toml"),
            r#"name = "agent"
auto_accept_trusted = false
default_timeout_seconds = 300

[provider]
provider_type = "open_a_i"
api_key = "sk-test"
default_model = "gpt-4"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.gpt-4]
name = "gpt-4"
max_tokens = 4096
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0
"#,
        )
        .await
        .unwrap();
        // No identity/ directory

        let registry = AgentRegistry::new(temp_dir.path().join("registry"));
        registry.init().await.unwrap();

        let result =
            AgentBuilder::build_from_directory(&agent_dir, "agent:v1", &registry, |_| {}).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("identity"));
    }

    #[tokio::test]
    async fn test_layer_digest_determinism() {
        let temp_dir = tempfile::tempdir().unwrap();
        let agent_dir = temp_dir.path().join("agent");
        create_test_agent_dir(&agent_dir).await.unwrap();

        let registry1 = AgentRegistry::new(temp_dir.path().join("registry1"));
        registry1.init().await.unwrap();

        let registry2 = AgentRegistry::new(temp_dir.path().join("registry2"));
        registry2.init().await.unwrap();

        let result1 = AgentBuilder::build_from_directory(&agent_dir, "a:v1", &registry1, |_| {})
            .await
            .unwrap();
        let result2 = AgentBuilder::build_from_directory(&agent_dir, "a:v1", &registry2, |_| {})
            .await
            .unwrap();

        // Same source → same layer digests
        let layers1 = result1.manifest.layers.unwrap();
        let layers2 = result2.manifest.layers.unwrap();
        assert_eq!(layers1.config, layers2.config);
        assert_eq!(layers1.identity, layers2.identity);
        assert_eq!(layers1.skills, layers2.skills);
    }
}
