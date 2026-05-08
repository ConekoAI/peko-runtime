//! Packager for creating portable agent packages
//!
//! Exports agents to `.agent` files (tar.gz archives with manifest)

use crate::identity::Identity;
use crate::extensions::mcp::protocol::config::{McpConfig, TransportType};
use crate::portable::manifest::AgentManifest;
use crate::types::agent::AgentConfig;
use anyhow::Context;
use std::collections::HashMap;
use std::path::Path;

/// Convert transport type to string representation
fn transport_to_string(transport: &TransportType) -> &'static str {
    match transport {
        TransportType::Stdio => "stdio",
        TransportType::Sse => "sse",
    }
}

/// Export options
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
    /// MCP config path (for bundling MCP servers)
    pub mcp_config_path: Option<std::path::PathBuf>,
    /// Universal Tools directory (for discovering tool versions)
    pub tools_dir: Option<std::path::PathBuf>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            encrypt: false,
            passphrase: None,

            include_sessions: false,     // Off by default (can be large)
            include_workspace: true,     // On by default (essential files)
            rotate_keys: false,
            description: None,
            output_path: None,
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
    /// Skills directory
    skills_dir: Option<std::path::PathBuf>,
    /// Workspace directory (SYSTEM.md, etc.)
    workspace_dir: Option<std::path::PathBuf>,
    /// Sessions directory (conversation history)
    sessions_dir: Option<std::path::PathBuf>,
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
        }
    }

    /// Set skills directory
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

        // 3. Export skills
        self.export_skills(&mut files, &mut manifest)
            .await
            .context("Failed to export skills")?;

        // 5. Export workspace (if included)
        if options.include_workspace {
            self.export_workspace(&mut files, &mut manifest)
                .await
                .context("Failed to export workspace")?;
        }

        // 6. Export sessions (if included)
        if options.include_sessions {
            self.export_sessions(&mut files, &mut manifest)
                .await
                .context("Failed to export sessions")?;
        }

        // 7. Sign manifest
        self.sign_manifest(&mut manifest)
            .context("Failed to sign manifest")?;

        // 11. Add manifest to files
        let manifest_toml = manifest.to_toml().context("Failed to serialize manifest")?;
        files.insert("manifest.toml".to_string(), manifest_toml.into_bytes());

        Ok((files, manifest))
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

        // Export prompts/personality
        let prompts = self.export_prompts();
        files.insert("config/prompts.toml".to_string(), prompts.into_bytes());
        manifest.add_file("config/prompts.toml", &files["config/prompts.toml"]);

        Ok(())
    }

    /// Export memory database
    /// Export skills
    /// Copies entire skill directories (not just .toml files)
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

    /// Recursively export a skill directory
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
            signatures: crate::portable::manifest::Signatures {
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
    fn make_portable_config(&self) -> AgentConfig {
        let mut portable = self.config.clone();

        // Remove sensitive data (API keys should be set on import)
        portable.provider.api_key = None;
        portable.provider.api_key_env = None;

        portable
    }

    /// Export prompts/personality
    fn export_prompts(&self) -> String {
        // Simple prompts export - can be extended
        let prompts = serde_json::json!({
            "system_prompt": format!("You are {}, a helpful AI assistant.", self.config.name),
            "extensions": self.config.extension_whitelist(),
        });

        toml::to_string_pretty(&prompts).unwrap_or_default()
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
