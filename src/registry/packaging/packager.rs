//! Packager for creating portable agent packages
//!
//! Exports agents to `.agent` files (tar.gz archives with manifest)
//!
//! # Deprecation notice
//!
//! Standalone `skills/` and `mcp/` layers are deprecated as of ADR-037.
//! Skills and MCP servers are now managed through the extension framework
//! and are recorded in `AgentManifest.extensions`. Legacy packages that
//! already contain these layers can still be imported, but new agent
//! exports no longer emit them. The deprecated fields and methods on
//! [`ExportOptions`] and [`Packager`] are retained only for backward
//! compatibility with team packaging, which still uses the skills layer.

use crate::identity::Identity;
use crate::registry::packaging::manifest::{AgentLayers, AgentManifest};
use crate::registry::packaging::types::{compute_digest, ExtensionRef, LayerType};
use crate::agents::agent_config::AgentConfig;
use anyhow::Context;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Export options
///
/// # Deprecated options
///
/// `mcp_config_path` and `tools_dir` are deprecated and no longer used by
/// agent packaging. They are retained on this struct for backward
/// compatibility with team packaging, which may still initialize them.
/// MCP servers and skills are now handled via `AgentManifest.extensions`.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Encrypt the package with a passphrase
    pub encrypt: bool,
    /// Passphrase for encryption (if encrypt is true)
    pub passphrase: Option<String>,

    /// Include session history (can be large)
    pub include_sessions: bool,
    /// Include workspace files (SYSTEM.md, AGENTS.md, etc.)
    pub include_workspace: bool,
    /// Rotate keys on import (create new DID)
    pub rotate_keys: bool,
    /// Description for the package
    pub description: Option<String>,
    /// Output path
    pub output_path: Option<String>,
    /// Embed extension packages in an `extensions/` layer (ADR-037).
    /// When true, each enabled non-built-in extension is exported as a
    /// `.ext` package inside the `.agent` archive.
    pub with_extensions: bool,
    /// **Deprecated:** MCP config path is no longer used by agent packaging.
    /// Retained for team-packaging backward compatibility.
    pub mcp_config_path: Option<std::path::PathBuf>,
    /// **Deprecated:** Universal Tools directory is no longer used by agent packaging.
    /// Retained for team-packaging backward compatibility.
    pub tools_dir: Option<std::path::PathBuf>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            encrypt: false,
            passphrase: None,

            include_sessions: false, // Off by default (can be large)
            include_workspace: true, // On by default (essential files)
            rotate_keys: false,
            description: None,
            output_path: None,
            with_extensions: false,
            mcp_config_path: None,
            tools_dir: None,
        }
    }
}

/// Packager for creating .agent packages
pub struct Packager {
    /// Agent configuration
    config: AgentConfig,
    /// Agent identity
    identity: Identity,
    /// Memory database path (if any)
    memory_path: Option<std::path::PathBuf>,
    /// **Deprecated:** Skills directory is no longer used by agent packaging.
    /// Retained for team-packaging backward compatibility.
    skills_dir: Option<std::path::PathBuf>,
    /// Workspace directory (SYSTEM.md, etc.)
    workspace_dir: Option<std::path::PathBuf>,
    /// Sessions directory (conversation history)
    sessions_dir: Option<std::path::PathBuf>,
    /// Extension references to record in the manifest
    extension_refs: Vec<ExtensionRef>,
    /// Embedded extension packages to include in the `extensions/` layer.
    /// Map of package-relative path (e.g. `extensions/docker-skill.ext`)
    /// to the raw `.ext` file bytes.
    embedded_extensions: HashMap<String, Vec<u8>>,
}

impl Packager {
    /// Create a new packager
    #[must_use]
    pub fn new(
        config: AgentConfig,
        identity: Identity,
        memory_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            config,
            identity,
            memory_path,
            skills_dir: None,
            workspace_dir: None,
            sessions_dir: None,
            extension_refs: Vec::new(),
            embedded_extensions: HashMap::new(),
        }
    }

    /// **Deprecated:** Set skills directory.
    ///
    /// Agent packaging no longer emits a `skills/` layer; skills are now
    /// managed as extensions via `AgentManifest.extensions`. This method
    /// is retained for backward compatibility with team packaging.
    pub fn with_skills_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.skills_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set workspace directory
    pub fn with_workspace_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.workspace_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set sessions directory
    pub fn with_sessions_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.sessions_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set extension references to record in the manifest
    pub fn with_extension_refs(mut self, refs: Vec<ExtensionRef>) -> Self {
        self.extension_refs = refs;
        self
    }

    /// Set embedded extension packages to include in the `extensions/` layer.
    /// Each entry is a package-relative path (e.g. `extensions/docker-skill.ext`)
    /// and the raw `.ext` file bytes.
    pub fn with_embedded_extensions(mut self, extensions: HashMap<String, Vec<u8>>) -> Self {
        self.embedded_extensions = extensions;
        self
    }

    /// Export the agent to a .agent package
    pub async fn export(&self, options: ExportOptions) -> anyhow::Result<std::path::PathBuf> {
        // Collect all files for the package
        let (files, _manifest) = self.collect_files(options.clone()).await?;

        // Create archive
        let output_path = self
            .create_archive(&files, &options)
            .await
            .context("Failed to create archive")?;

        Ok(output_path)
    }

    /// Collect all files for the package without creating archive
    /// Returns the files map and the manifest (for team packaging)
    pub async fn collect_files(
        &self,
        options: ExportOptions,
    ) -> anyhow::Result<(HashMap<String, Vec<u8>>, AgentManifest)> {
        let mut manifest = AgentManifest::new(
            &self.config.name,
            "1.0.0", // Package version
            &self.identity.did,
        );

        // Set description
        if let Some(ref desc) = options.description {
            manifest.agent.description = Some(desc.clone());
        } else if let Some(ref desc) = &self.config.description {
            manifest.agent.description = Some(desc.clone());
        }

        // Collect files to package
        let mut files: HashMap<String, Vec<u8>> = HashMap::new();

        // 1. Export identity
        self.export_identity(&mut files, &mut manifest, &options)
            .await
            .context("Failed to export identity")?;

        // 2. Export configuration
        self.export_config(&mut files, &mut manifest)
            .context("Failed to export config")?;

        // 3. Export skills (legacy — only emitted when `skills_dir` is set by team packaging)
        self.export_skills(&mut files, &mut manifest)
            .await
            .context("Failed to export skills")?;

        // 4. Record extension references in manifest
        manifest.extensions = self.extension_refs.clone();

        // 5. Embed extension packages (if --with-extensions)
        if options.with_extensions {
            for (path, content) in &self.embedded_extensions {
                files.insert(path.clone(), content.clone());
                manifest.add_file(path, content);
            }
        }

        // 6. Export workspace (if included)
        if options.include_workspace {
            self.export_workspace(&mut files, &mut manifest)
                .await
                .context("Failed to export workspace")?;
        }

        // 7. Export sessions (if included)
        if options.include_sessions {
            self.export_sessions(&mut files, &mut manifest)
                .await
                .context("Failed to export sessions")?;
        }

        // 8. Compute layer digests from collected files
        manifest.layers = Some(Self::compute_layers(&files)?);

        // 9. Sign manifest
        self.sign_manifest(&mut manifest)
            .context("Failed to sign manifest")?;

        // 10. Add manifest to files
        let manifest_toml = manifest.to_toml().context("Failed to serialize manifest")?;
        files.insert("manifest.toml".to_string(), manifest_toml.into_bytes());

        Ok((files, manifest))
    }

    /// Compute layer digests from the collected files map.
    ///
    /// Groups files by their top-level directory prefix (config/, identity/, etc.),
    /// builds a gzipped tarball for each group, and computes its SHA-256 digest.
    ///
    /// `LayerType::Skills` and `LayerType::Mcp` are retained here so that legacy
    /// packages containing those prefixes still have their digests computed
    /// correctly; current agent exports no longer emit those layers unless
    /// team packaging explicitly provides a skills directory.
    fn compute_layers(files: &HashMap<String, Vec<u8>>) -> anyhow::Result<AgentLayers> {
        let mut layers = AgentLayers::default();

        let layer_types = [
            (LayerType::Config, "config"),
            (LayerType::Identity, "identity"),
            (LayerType::Skills, "skills"),
            (LayerType::Workspace, "workspace"),
            (LayerType::Sessions, "sessions"),
            (LayerType::Mcp, "mcp"),
            (LayerType::Extensions, "extensions"),
        ];

        for (layer_type, prefix) in layer_types {
            // Collect files for this layer prefix
            let mut layer_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            for (path, content) in files {
                if path.starts_with(&format!("{prefix}/")) {
                    let layer_path = path.strip_prefix(&format!("{prefix}/")).unwrap_or(path);
                    layer_files.insert(layer_path.to_string(), content.clone());
                }
            }

            if !layer_files.is_empty() {
                let digest = Self::build_layer_digest(&layer_files)?;
                match layer_type {
                    LayerType::Config => layers.config = Some(digest),
                    LayerType::Identity => layers.identity = Some(digest),
                    LayerType::Skills => layers.skills = Some(digest),
                    LayerType::Workspace => layers.workspace = Some(digest),
                    LayerType::Sessions => layers.sessions = Some(digest),
                    LayerType::Mcp => layers.mcp = Some(digest),
                    LayerType::Extensions => {
                        // Extensions layer is optional and appears only when
                        // extensions are embedded via --with-extensions.
                        layers.extensions = Some(digest);
                    }
                    LayerType::TeamConfig => {
                        // TeamConfig is not part of an agent manifest;
                        // it only appears in team registry manifests.
                    }
                }
            }
        }

        Ok(layers)
    }

    /// Build a gzipped tarball from a map of files and compute its digest.
    fn build_layer_digest(files: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<String> {
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
        Ok(compute_digest(&buf))
    }

    /// Export identity files
    async fn export_identity(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
        _options: &ExportOptions,
    ) -> anyhow::Result<()> {
        // Export DID document
        let did_doc = serde_json::to_vec_pretty(&self.identity.to_did_document()?)?;
        files.insert("identity/did.json".to_string(), did_doc);
        manifest.add_file("identity/did.json", &files["identity/did.json"]);

        // Export keys directly from the identity (we already have them in memory)
        let keypair = self
            .identity
            .keypair
            .as_ref()
            .context("Identity has no keypair")?;
        let key_export = keypair.export();
        let key_data = serde_json::to_vec(&key_export)?;

        files.insert("identity/keys.enc".to_string(), key_data);
        manifest.add_file("identity/keys.enc", &files["identity/keys.enc"]);

        Ok(())
    }

    /// Export configuration
    fn export_config(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
    ) -> anyhow::Result<()> {
        // Create portable config (strips runtime-specific paths)
        let portable_config = self.make_portable_config();

        let config_toml = toml::to_string_pretty(&portable_config)?;
        files.insert("config/agent.toml".to_string(), config_toml.into_bytes());
        manifest.add_file("config/agent.toml", &files["config/agent.toml"]);

        Ok(())
    }

    /// **Deprecated:** Export skills from `skills_dir`.
    ///
    /// This method is kept for backward compatibility with team packaging.
    /// Agent packaging no longer sets `skills_dir`, so new agent exports
    /// do not contain a `skills/` layer. Skills are now managed as
    /// extensions via `AgentManifest.extensions`.
    async fn export_skills(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
    ) -> anyhow::Result<()> {
        if let Some(skills_dir) = &self.skills_dir {
            if skills_dir.exists() {
                let mut entries = tokio::fs::read_dir(skills_dir).await?;

                while let Some(entry) = entries.next_entry().await? {
                    let path = entry.path();
                    // Skills are directories containing SKILL.md
                    if path.is_dir() {
                        let skill_name = path.file_name().unwrap().to_string_lossy();
                        self.export_skill_dir(
                            &path,
                            &format!("skills/{skill_name}"),
                            files,
                            manifest,
                        )
                        .await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// **Deprecated:** Recursively export a skill directory.
    ///
    /// This method is kept for backward compatibility with team packaging.
    async fn export_skill_dir(
        &self,
        src_dir: &std::path::Path,
        package_prefix: &str,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
    ) -> anyhow::Result<()> {
        let mut entries = tokio::fs::read_dir(src_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            let package_path = format!("{package_prefix}/{file_name}");

            if src_path.is_dir() {
                // Recurse into subdirectories
                Box::pin(self.export_skill_dir(&src_path, &package_path, files, manifest)).await?;
            } else {
                // Read and add file
                let content = tokio::fs::read(&src_path).await?;
                files.insert(package_path.clone(), content);
                manifest.add_file(&package_path, &files[&package_path]);
            }
        }

        Ok(())
    }

    /// Export workspace files (SYSTEM.md, AGENTS.md, etc.)
    async fn export_workspace(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
    ) -> anyhow::Result<()> {
        if let Some(workspace_dir) = &self.workspace_dir {
            if workspace_dir.exists() {
                self.export_dir_recursive(workspace_dir, "workspace", files, manifest)
                    .await?;
            }
        }
        Ok(())
    }

    /// Export session history
    async fn export_sessions(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
    ) -> anyhow::Result<()> {
        if let Some(sessions_dir) = &self.sessions_dir {
            if sessions_dir.exists() {
                self.export_dir_recursive(sessions_dir, "sessions", files, manifest)
                    .await?;
            }
        }
        Ok(())
    }

    /// Generic recursive directory export helper
    async fn export_dir_recursive(
        &self,
        src_dir: &std::path::Path,
        package_prefix: &str,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
    ) -> anyhow::Result<()> {
        let mut entries = tokio::fs::read_dir(src_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            let package_path = format!("{package_prefix}/{file_name}");

            if src_path.is_dir() {
                // Recurse into subdirectories
                Box::pin(self.export_dir_recursive(&src_path, &package_path, files, manifest))
                    .await?;
            } else {
                // Read and add file
                let content = tokio::fs::read(&src_path).await?;
                files.insert(package_path.clone(), content);
                manifest.add_file(&package_path, &files[&package_path]);
            }
        }

        Ok(())
    }

    /// Sign manifest
    fn sign_manifest(&self, manifest: &mut AgentManifest) -> anyhow::Result<()> {
        // Create manifest without signature for signing
        let manifest_for_signing = AgentManifest {
            signatures: crate::registry::packaging::manifest::Signatures {
                manifest: String::new(),
                algorithm: "ed25519".to_string(),
            },
            ..manifest.clone()
        };

        let manifest_toml = manifest_for_signing.to_toml()?;

        // Sign with agent's DID key (from memory, not storage)
        let keypair = self
            .identity
            .keypair
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Identity has no keypair"))?;

        let signature = keypair.sign(manifest_toml.as_bytes());

        // Use standard base64 encoding (URL-safe without padding)
        use base64::Engine;
        manifest.signatures.manifest =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
        manifest.signatures.algorithm = "ed25519".to_string();

        Ok(())
    }

    /// Create the final archive
    async fn create_archive(
        &self,
        files: &HashMap<String, Vec<u8>>,
        options: &ExportOptions,
    ) -> anyhow::Result<std::path::PathBuf> {
        // Determine output path
        let output_path = if let Some(path) = &options.output_path {
            std::path::PathBuf::from(path)
        } else {
            std::path::PathBuf::from(format!("{}.agent", self.config.name))
        };

        // Ensure parent directory exists
        if let Some(parent) = output_path.parent() {
            if !parent.exists() {
                tokio::fs::create_dir_all(parent).await.with_context(|| {
                    format!("Failed to create output directory: {}", parent.display())
                })?;
            }
        }

        // Create tar.gz
        let tar_gz = std::fs::File::create(&output_path)?;
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Add files
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();

            tar.append(&header, content.as_slice())?;
        }

        // Finish archive
        tar.finish()?;

        Ok(output_path)
    }

    /// Make config portable (remove runtime-specific paths)
    ///
    /// v3: nothing to strip — API keys live in the OS keychain
    /// (referenced by the catalog's `provider_id`), not on the
    /// agent config. Just hand the config back unchanged.
    fn make_portable_config(&self) -> AgentConfig {
        self.config.clone()
    }
}

/// Convenience function to export an agent
pub async fn export_agent(
    config: AgentConfig,
    identity: Identity,
    memory_path: Option<std::path::PathBuf>,
    options: ExportOptions,
) -> anyhow::Result<std::path::PathBuf> {
    let packager = Packager::new(config, identity, memory_path);
    packager.export(options).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests would need a mock identity setup
    // For now, just test the options struct

    #[test]
    fn test_export_options_default() {
        let opts = ExportOptions::default();
        assert!(!opts.encrypt);
        assert!(!opts.include_sessions); // Default: false (large)
        assert!(opts.include_workspace); // Default: true (essential)
        assert!(!opts.rotate_keys);
    }
}

    #[test]
    fn test_export_options_with_extensions_default_false() {
        let opts = ExportOptions::default();
        assert!(!opts.with_extensions);
    }

    #[test]
    fn test_compute_layers_includes_extensions_digest() {
        let mut files = std::collections::HashMap::new();
        files.insert("config/agent.toml".to_string(), b"[agent]\nname = \"test\"".to_vec());
        files.insert(
            "extensions/docker-skill.ext".to_string(),
            b"fake ext bytes".to_vec(),
        );

        let layers = Packager::compute_layers(&files).unwrap();
        assert!(layers.config.is_some());
        assert!(layers.extensions.is_some());
        // skills/mcp should not be present in a clean ADR-037 package
        assert!(layers.skills.is_none());
        assert!(layers.mcp.is_none());
    }

    #[test]
    fn test_compute_layers_skills_legacy_backward_compat() {
        let mut files = std::collections::HashMap::new();
        files.insert("config/agent.toml".to_string(), b"[agent]\nname = \"test\"".to_vec());
        files.insert(
            "skills/legacy-skill/SKILL.md".to_string(),
            b"# Legacy skill".to_vec(),
        );

        let layers = Packager::compute_layers(&files).unwrap();
        assert!(layers.config.is_some());
        assert!(layers.skills.is_some());
        assert!(layers.extensions.is_none());
    }
