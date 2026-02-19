//! Packager for creating portable agent packages
//!
//! Exports agents to `.agent` files (tar.gz archives with manifest)

use crate::identity::{Identity, KeyStorage};
use crate::portable::{
    crypto::{encrypt_with_passphrase, serialize_encrypted},
    manifest::AgentManifest,
};
use crate::types::agent::AgentConfig;
use std::collections::HashMap;
use std::path::Path;

/// Export options
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Encrypt the package with a passphrase
    pub encrypt: bool,
    /// Passphrase for encryption (if encrypt is true)
    pub passphrase: Option<String>,
    /// Include memory database
    pub include_memory: bool,
    /// Rotate keys on import (create new DID)
    pub rotate_keys: bool,
    /// Description for the package
    pub description: Option<String>,
    /// Output path
    pub output_path: Option<String>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            encrypt: false,
            passphrase: None,
            include_memory: true,
            rotate_keys: false,
            description: None,
            output_path: None,
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
}

impl Packager {
    /// Create a new packager
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
        }
    }

    /// Set skills directory
    pub fn with_skills_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.skills_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Export the agent to a .agent package
    pub async fn export(&self,
        options: ExportOptions,
    ) -> anyhow::Result<std::path::PathBuf> {
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
        self.export_identity(&mut files, &mut manifest, &options).await?;

        // 2. Export configuration
        self.export_config(&mut files, &mut manifest)?;

        // 3. Export memory (if included)
        if options.include_memory {
            self.export_memory(&mut files, &mut manifest, &options).await?;
        }

        // 4. Export skills
        self.export_skills(&mut files, &mut manifest).await?;

        // 5. Build capabilities and tools lists
        self.build_capabilities(&mut manifest);

        // 6. Sign manifest
        self.sign_manifest(&mut manifest)?;

        // 7. Add manifest to files
        let manifest_toml = manifest.to_toml()?;
        files.insert("manifest.toml".to_string(), manifest_toml.into_bytes());

        // 8. Create archive
        let output_path = self.create_archive(&files, &options).await?;

        Ok(output_path)
    }

    /// Export identity files
    async fn export_identity(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
        options: &ExportOptions,
    ) -> anyhow::Result<()> {
        // Export DID document
        let did_doc = serde_json::to_vec_pretty(&self.identity.to_did_document()?)?;
        files.insert("identity/did.json".to_string(), did_doc);
        manifest.add_file("identity/did.json", &files["identity/did.json"]);

        // Export keys (potentially encrypted)
        let key_storage = KeyStorage::new()?;
        let key_export = key_storage.export_keys(&self.identity.did)?;

        let key_data = if options.encrypt {
            if let Some(passphrase) = &options.passphrase {
                let encrypted = encrypt_with_passphrase(
                    &serde_json::to_vec(&key_export)?,
                    passphrase,
                )?;

                // Update manifest with encryption info
                let kdf_params: HashMap<String, String> = [
                    ("memory_cost".to_string(), "65536".to_string()),
                    ("time_cost".to_string(), "3".to_string()),
                    ("parallelism".to_string(), "4".to_string()),
                ]
                .into_iter()
                .collect();
                manifest.set_encrypted("argon2id", kdf_params);

                serialize_encrypted(&encrypted)
            } else {
                return Err(anyhow::anyhow!("Encryption enabled but no passphrase provided"));
            }
        } else {
            serde_json::to_vec(&key_export)?
        };

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
    async fn export_memory(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut AgentManifest,
        _options: &ExportOptions,
    ) -> anyhow::Result<()> {
        if let Some(memory_path) = &self.memory_path {
            if memory_path.exists() {
                let memory_data = tokio::fs::read(memory_path).await?;

                manifest.memory.size_bytes = memory_data.len() as u64;
                manifest.memory.encrypted = false; // TODO: Support memory encryption

                files.insert("memory/memory.db".to_string(), memory_data);
                manifest.add_file("memory/memory.db", &files["memory/memory.db"]);
            }
        }

        Ok(())
    }

    /// Export skills
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
                    if path.extension().map(|e| e == "toml").unwrap_or(false) {
                        let content = tokio::fs::read(&path).await?;
                        let file_name = path.file_name().unwrap().to_string_lossy();
                        let package_path = format!("config/skills/{}", file_name);

                        files.insert(package_path.clone(), content);
                        manifest.add_file(&package_path, &files[&package_path]);
                    }
                }
            }
        }

        Ok(())
    }

    /// Build capabilities list
    fn build_capabilities(&self,
        manifest: &mut AgentManifest,
    ) {
        let names: Vec<String> = self.config.capabilities.iter()
            .map(|c| c.name.clone())
            .collect();

        let versions: HashMap<String, String> = self.config.capabilities.iter()
            .map(|c| (c.name.clone(), c.version.clone()))
            .collect();

        manifest.capabilities.names = names;
        manifest.capabilities.versions = Some(versions);
    }

    /// Sign the manifest
    fn sign_manifest(
        &self,
        manifest: &mut AgentManifest,
    ) -> anyhow::Result<()> {
        // Create manifest without signature for signing
        let manifest_for_signing = AgentManifest {
            signatures: crate::portable::manifest::Signatures {
                manifest: String::new(),
                algorithm: "ed25519".to_string(),
            },
            ..manifest.clone()
        };

        let manifest_toml = manifest_for_signing.to_toml()?;

        // Sign with agent's DID key
        let key_storage = KeyStorage::new()?;
        let identity = key_storage.load(&self.identity.did)?;
        let keypair = identity.keypair.as_ref().ok_or_else(|| anyhow::anyhow!("No keypair"))?;

        use ed25519_dalek::Signer as _;
        let signature = keypair.sign(manifest_toml.as_bytes());

        // Use standard base64 encoding (URL-safe without padding)
        use base64::Engine;
        manifest.signatures.manifest = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
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
    fn make_portable_config(&self,
    ) -> AgentConfig {
        let mut portable = self.config.clone();

        // Remove sensitive data (API keys should be set on import)
        portable.provider.api_key = None;
        portable.provider.api_key_env = None;

        // Remove runtime-specific paths
        if let Some(memory) = &mut portable.memory {
            memory.database_path = None; // Will be set on import
        }

        // Remove Coneko-specific config (can be reconfigured on import)
        portable.coneko = None;

        portable
    }

    /// Export prompts/personality
    fn export_prompts(&self,
    ) -> String {
        // Simple prompts export - can be extended
        let prompts = serde_json::json!({
            "system_prompt": format!("You are {}, a helpful AI assistant.", self.config.name),
            "capabilities": self.config.capabilities.iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
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
    use crate::identity::Identity;
    use crate::types::agent::AgentConfig;

    // Note: These tests would need a mock identity setup
    // For now, just test the options struct

    #[test]
    fn test_export_options_default() {
        let opts = ExportOptions::default();
        assert!(!opts.encrypt);
        assert!(opts.include_memory);
        assert!(!opts.rotate_keys);
    }
}
