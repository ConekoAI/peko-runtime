//! Unpackager for importing portable agent packages
//!
//! Extracts and imports .agent files into the local Pekobot runtime
#![allow(dead_code)]

use crate::identity::{storage::KeyStorage, Identity, KeyPairExport};
use crate::mcp::config::{McpConfig, McpServerConfig, TransportType};
use crate::portable::{
    crypto::{decrypt_with_passphrase, deserialize_encrypted},
    manifest::{AgentManifest, McpManifestEntry},
    validation::{validate_package, ValidationResult},
};
use crate::types::agent::AgentConfig;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// Parse transport type from string
fn parse_transport(transport: &str) -> TransportType {
    match transport {
        "sse" => TransportType::Sse,
        _ => TransportType::Stdio,
    }
}

/// MCP installation result
#[derive(Debug, Clone)]
pub struct McpInstallResult {
    /// Server name
    pub name: String,
    /// Binary path (if bundled)
    pub binary_path: Option<std::path::PathBuf>,
    /// Whether it was installed from bundle
    pub from_bundle: bool,
}

/// Import options
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// New name for the imported agent (optional)
    pub new_name: Option<String>,
    /// Passphrase for decryption (if encrypted)
    pub passphrase: Option<String>,
    /// Rotate keys (generate new DID)
    pub rotate_keys: bool,
    /// Import memory database (deprecated)
    pub import_memory: bool,
    /// Import session history
    pub import_sessions: bool,
    /// Import workspace files
    pub import_workspace: bool,
    /// Import MCP servers (extract bundled binaries)
    pub import_mcp: bool,
    /// Install tools from registry references
    pub install_tools_from_registry: bool,
    /// Skip validation (not recommended)
    pub skip_validation: bool,
    /// Force import even if DID exists
    pub force: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            new_name: None,
            passphrase: None,
            rotate_keys: false,
            import_memory: true,       // Deprecated but kept for compat
            import_sessions: true,     // Default: import if present
            import_workspace: true,    // Default: import if present
            import_mcp: true,          // Import MCP servers by default
            install_tools_from_registry: false, // Off by default (requires network)
            skip_validation: false,
            force: false,
        }
    }
}

/// Import result
#[derive(Debug, Clone)]
pub struct ImportResult {
    /// New agent name
    pub name: String,
    /// Agent DID
    pub did: String,
    /// Whether keys were rotated
    pub keys_rotated: bool,
    /// Path to imported memory database (deprecated)
    pub memory_path: Option<std::path::PathBuf>,
    /// Path to imported workspace
    pub workspace_path: Option<std::path::PathBuf>,
    /// Path to imported sessions
    pub sessions_path: Option<std::path::PathBuf>,
    /// Path to agent config
    pub config_path: std::path::PathBuf,
    /// MCP servers installed
    pub mcp_servers: Vec<McpInstallResult>,
    /// Tools installed from registry
    pub tools_installed: Vec<String>,
    /// Validation result
    pub validation: ValidationResult,
}

/// Unpackager for importing .agent packages
pub struct Unpackager {
    /// Package file path
    package_path: std::path::PathBuf,
    /// Base directory for imported agents
    base_dir: std::path::PathBuf,
}

impl Unpackager {
    /// Create a new unpackager
    pub fn new(package_path: impl AsRef<Path>) -> Self {
        Self {
            package_path: package_path.as_ref().to_path_buf(),
            base_dir: dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("pekobot"),
        }
    }

    /// Set custom base directory
    pub fn with_base_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.base_dir = dir.as_ref().to_path_buf();
        self
    }

    /// Inspect a package without importing
    pub async fn inspect(
        &self,
        passphrase: Option<&str>,
    ) -> anyhow::Result<(AgentManifest, ValidationResult)> {
        // Extract package
        let files = self.extract_package().await?;

        // Parse manifest
        let manifest_bytes = files
            .get("manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing manifest.toml in package"))?;
        let manifest_str = std::str::from_utf8(manifest_bytes)?;
        let manifest = AgentManifest::from_toml(manifest_str)?;

        // Validate
        let validation = validate_package(&manifest, &files);

        // Check if encrypted and we have passphrase
        if manifest.identity.encrypted && passphrase.is_none() {
            return Err(anyhow::anyhow!(
                "Package is encrypted but no passphrase provided"
            ));
        }

        Ok((manifest, validation))
    }

    /// Import the package from a file
    pub async fn import(&self, options: ImportOptions) -> anyhow::Result<ImportResult> {
        // Extract package
        let files = self.extract_package().await?;
        
        // Process the import with extracted files
        self.import_from_files(files, options).await
    }

    /// Import from pre-extracted files (used by team unpackager)
    pub(crate) async fn import_from_files(
        &self,
        files: HashMap<String, Vec<u8>>,
        options: ImportOptions,
    ) -> anyhow::Result<ImportResult> {
        // Parse manifest
        let manifest = self.parse_manifest(&files)?;

        // Validate unless skipped
        let validation = if options.skip_validation {
            ValidationResult::success()
        } else {
            validate_package(&manifest, &files)
        };

        if !validation.is_valid() && !options.force {
            return Err(anyhow::anyhow!(
                "Package validation failed. Use --force to import anyway.\n{}",
                validation.error_report()
            ));
        }

        // Determine agent name
        let name = options
            .new_name
            .clone()
            .unwrap_or_else(|| manifest.agent.name.clone());

        // Import identity (decrypt if needed)
        let identity = self.import_identity(&files, &manifest, &options).await?;

        // Import configuration
        let config = self.import_config(&files, &manifest, &name, &identity)?;

        // Import memory
        let memory_path = if options.import_memory {
            Some(self.import_memory(&files, &identity, &manifest).await?)
        } else {
            None
        };

        // Import skills
        self.import_skills(&files).await?;

        // Import workspace (if present in package)
        if options.import_workspace {
            self.import_workspace(&files, &name).await?;
        }

        // Import sessions (if present in package)
        if options.import_sessions {
            self.import_sessions(&files, &name).await?;
        }

        // Import MCP servers (if enabled and present)
        let mcp_servers = if options.import_mcp {
            self.import_mcp_servers(&files, &manifest).await?
        } else {
            Vec::new()
        };

        // Install tools from registry (if enabled)
        let tools_installed = if options.install_tools_from_registry {
            self.install_tools_from_registry(&manifest).await?
        } else {
            Vec::new()
        };

        // Save config
        let config_path = self.save_config(&config, &name).await?;

        // Compute workspace and sessions paths for result
        let team = "default";
        let workspace_path = if options.import_workspace {
            Some(
                dirs::data_dir()
                    .map(|d| d.join("pekobot").join("workspaces").join(team).join(&name))
                    .unwrap_or_else(|| self.base_dir.join("workspaces").join(&name))
            )
        } else {
            None
        };
        
        let sessions_path = if options.import_sessions {
            Some(
                dirs::data_dir()
                    .map(|d| d.join("pekobot").join("sessions").join(team).join(&name))
                    .unwrap_or_else(|| self.base_dir.join("sessions").join(&name))
            )
        } else {
            None
        };

        Ok(ImportResult {
            name,
            did: identity.did,
            keys_rotated: options.rotate_keys,
            memory_path,
            workspace_path,
            sessions_path,
            config_path,
            mcp_servers,
            tools_installed,
            validation,
        })
    }

    /// Extract package to memory
    async fn extract_package(&self) -> anyhow::Result<HashMap<String, Vec<u8>>> {
        let file = std::fs::File::open(&self.package_path)?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);

        let mut files = HashMap::new();

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            let path_str = path.to_string_lossy().to_string();

            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;

            files.insert(path_str, content);
        }

        Ok(files)
    }

    /// Parse manifest from files
    fn parse_manifest(&self, files: &HashMap<String, Vec<u8>>) -> anyhow::Result<AgentManifest> {
        let manifest_bytes = files
            .get("manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing manifest.toml"))?;
        let manifest_str = std::str::from_utf8(manifest_bytes)?;
        AgentManifest::from_toml(manifest_str)
    }

    /// Import identity (decrypt keys if needed)
    async fn import_identity(
        &self,
        files: &HashMap<String, Vec<u8>>,
        manifest: &AgentManifest,
        options: &ImportOptions,
    ) -> anyhow::Result<Identity> {
        let did_doc_bytes = files
            .get("identity/did.json")
            .ok_or_else(|| anyhow::anyhow!("Missing identity/did.json"))?;
        let did_doc: crate::identity::DIDDocument = serde_json::from_slice(did_doc_bytes)?;

        if options.rotate_keys {
            // Generate new identity
            let new_identity =
                Identity::new(&manifest.agent.name, crate::identity::did::DIDScope::Local).await?;

            // Store new keys
            let key_storage = KeyStorage::new()?;
            key_storage.store_identity(&new_identity).await?;

            Ok(new_identity)
        } else {
            // Import existing keys
            let encrypted_keys = files
                .get("identity/keys.enc")
                .ok_or_else(|| anyhow::anyhow!("Missing identity/keys.enc"))?;

            let key_data = if manifest.identity.encrypted {
                if let Some(passphrase) = &options.passphrase {
                    let encrypted = deserialize_encrypted(encrypted_keys)?;
                    decrypt_with_passphrase(&encrypted, passphrase)?
                } else {
                    return Err(anyhow::anyhow!(
                        "Keys are encrypted but no passphrase provided"
                    ));
                }
            } else {
                encrypted_keys.clone()
            };

            let key_export: KeyPairExport = serde_json::from_slice(&key_data)?;

            // Reconstruct identity
            let identity = Identity::from_did_document_and_key(did_doc, key_export)?;

            // Check if DID already exists locally
            let key_storage = KeyStorage::new()?;
            if key_storage.exists(&identity.did) && !options.force {
                return Err(anyhow::anyhow!(
                    "DID {} already exists locally. Use --force to overwrite or --rotate-keys to generate new identity.",
                    identity.did
                ));
            }

            // Store identity
            key_storage.store_identity(&identity).await?;

            Ok(identity)
        }
    }

    /// Import configuration
    fn import_config(
        &self,
        files: &HashMap<String, Vec<u8>>,
        _manifest: &AgentManifest,
        new_name: &str,
        _identity: &Identity,
    ) -> anyhow::Result<AgentConfig> {
        let config_bytes = files
            .get("config/agent.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing config/agent.toml"))?;
        let config_str = std::str::from_utf8(config_bytes)?;
        let mut config: AgentConfig = toml::from_str(config_str)?;

        // Update runtime-specific fields
        config.name = new_name.to_string();

        // Note: Memory import is no longer supported as core memory has been deprecated.
        // The config.memory field no longer exists in AgentConfig.

        Ok(config)
    }

    /// Import memory database
    ///
    /// Deprecated: Core memory has been removed. Memory data in legacy packages
    /// is no longer imported. This method returns a path but the data is not
    /// actually imported.
    #[deprecated(since = "0.9.0", note = "Core memory is deprecated. Use external MCP memory servers instead.")]
    async fn import_memory(
        &self,
        files: &HashMap<String, Vec<u8>>,
        identity: &Identity,
        manifest: &AgentManifest,
    ) -> anyhow::Result<std::path::PathBuf> {
        // Deprecated: Core memory has been removed.
        // Log a warning but don't actually import memory data.
        tracing::warn!(
            "Memory import is deprecated. Found {} bytes of memory data in package ({} entries), but core memory is no longer supported. Use external MCP memory servers instead.",
            manifest.memory.size_bytes,
            manifest.memory.entry_count.unwrap_or(0)
        );

        // Return a path anyway to avoid breaking the result type,
        // but the data is not actually imported.
        let memory_path = self.get_memory_path(&identity.did);
        Ok(memory_path)
    }

    /// Import skills
    async fn import_skills(&self, files: &HashMap<String, Vec<u8>>) -> anyhow::Result<()> {
        let skills_dir = self.base_dir.join("skills");
        tokio::fs::create_dir_all(&skills_dir).await?;

        for (path, content) in files {
            // Handle new path structure: skills/{name}/SKILL.md
            if path.starts_with("skills/") {
                let file_name = path.strip_prefix("skills/").unwrap_or(path);
                let dest_path = skills_dir.join(file_name);

                if let Some(parent) = dest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                tokio::fs::write(dest_path, content).await?;
            }
        }

        Ok(())
    }

    /// Import workspace files
    async fn import_workspace(&self, files: &HashMap<String, Vec<u8>>, agent_name: &str) -> anyhow::Result<()> {
        // Determine team (default to "default" if not specified)
        let team = "default"; // Could be extracted from config
        
        let workspace_dir = dirs::data_dir()
            .map(|d| d.join("pekobot").join("workspaces").join(team).join(agent_name))
            .unwrap_or_else(|| self.base_dir.join("workspaces").join(agent_name));
            
        tokio::fs::create_dir_all(&workspace_dir).await?;

        for (path, content) in files {
            if path.starts_with("workspace/") {
                let file_name = path.strip_prefix("workspace/").unwrap_or(path);
                let dest_path = workspace_dir.join(file_name);

                if let Some(parent) = dest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                tokio::fs::write(dest_path, content).await?;
            }
        }

        Ok(())
    }

    /// Import session history
    async fn import_sessions(&self, files: &HashMap<String, Vec<u8>>, agent_name: &str) -> anyhow::Result<()> {
        // Determine team (default to "default")
        let team = "default";
        
        let sessions_dir = dirs::data_dir()
            .map(|d| d.join("pekobot").join("sessions").join(team).join(agent_name))
            .unwrap_or_else(|| self.base_dir.join("sessions").join(agent_name));
            
        tokio::fs::create_dir_all(&sessions_dir).await?;

        for (path, content) in files {
            if path.starts_with("sessions/") {
                let file_name = path.strip_prefix("sessions/").unwrap_or(path);
                let dest_path = sessions_dir.join(file_name);

                if let Some(parent) = dest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                tokio::fs::write(dest_path, content).await?;
            }
        }

        Ok(())
    }

    /// Save agent config
    async fn save_config(
        &self,
        config: &AgentConfig,
        name: &str,
    ) -> anyhow::Result<std::path::PathBuf> {
        let agent_dir = self.base_dir.join("agents").join(name);
        tokio::fs::create_dir_all(&agent_dir).await?;

        let config_path = agent_dir.join("config.toml");
        let config_toml = toml::to_string_pretty(config)?;

        tokio::fs::write(&config_path, config_toml).await?;

        Ok(config_path)
    }

    /// Get memory path for a DID
    fn get_memory_path(&self, did: &str) -> std::path::PathBuf {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("pekobot")
            .join("memory");

        // Hash DID to create safe filename
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        did.hash(&mut hasher);
        let hash = hasher.finish();

        data_dir.join(format!("{hash:x}.db"))
    }

    /// Import MCP servers from package
    async fn import_mcp_servers(
        &self,
        files: &HashMap<String, Vec<u8>>,
        manifest: &AgentManifest,
    ) -> anyhow::Result<Vec<McpInstallResult>> {
        if manifest.mcp.servers.is_empty() {
            return Ok(Vec::new());
        }

        let mcp_tools_dir = self.base_dir.join("tools").join("mcp");
        tokio::fs::create_dir_all(&mcp_tools_dir).await?;

        // Load existing MCP config once
        let mcp_config_path = self.base_dir.join("mcp.toml");
        let mut mcp_config = Self::load_mcp_config(&mcp_config_path).await?;

        let mut results = Vec::new();
        for server_entry in &manifest.mcp.servers {
            let result = self.import_single_mcp_server(
                server_entry,
                files,
                &mcp_tools_dir,
                &mut mcp_config,
            ).await?;
            results.push(result);
        }

        // Save config once after all servers processed
        let config_toml = mcp_config.to_toml()?;
        tokio::fs::write(&mcp_config_path, config_toml).await?;

        Ok(results)
    }

    /// Load MCP config from file or create default
    async fn load_mcp_config(path: &std::path::PathBuf) -> anyhow::Result<McpConfig> {
        if path.exists() {
            McpConfig::from_file(path).await.or_else(|e| {
                tracing::warn!("Failed to load existing MCP config: {}. Creating new.", e);
                Ok(McpConfig::default())
            })
        } else {
            Ok(McpConfig::default())
        }
    }

    /// Import a single MCP server
    async fn import_single_mcp_server(
        &self,
        entry: &McpManifestEntry,
        files: &HashMap<String, Vec<u8>>,
        mcp_tools_dir: &std::path::Path,
        mcp_config: &mut McpConfig,
    ) -> anyhow::Result<McpInstallResult> {
        let server_dir = mcp_tools_dir.join(&entry.name);
        
        // Extract binary if bundled
        let (binary_path, from_bundle) = if entry.bundled {
            self.extract_mcp_binary(entry, files, &server_dir).await?
        } else {
            (None, false)
        };

        // Create and add server config
        let server_config = self.create_mcp_server_config(entry, &server_dir, &binary_path, from_bundle);
        mcp_config.remove_server(&entry.name);
        mcp_config.add_server(server_config);

        Ok(McpInstallResult {
            name: entry.name.clone(),
            binary_path,
            from_bundle,
        })
    }

    /// Extract MCP binary from package
    async fn extract_mcp_binary(
        &self,
        entry: &McpManifestEntry,
        files: &HashMap<String, Vec<u8>>,
        server_dir: &std::path::Path,
    ) -> anyhow::Result<(Option<std::path::PathBuf>, bool)> {
        let Some(ref bundle_path) = entry.bundle_path else {
            return Ok((None, false));
        };
        
        let Some(binary_data) = files.get(bundle_path) else {
            tracing::warn!("Bundled MCP server '{}' missing binary data at {}", entry.name, bundle_path);
            return Ok((None, false));
        };

        tokio::fs::create_dir_all(server_dir).await?;
        
        let bin_path = server_dir.join("bin");
        tokio::fs::write(&bin_path, binary_data).await?;
        
        #[cfg(unix)]
        Self::set_executable_permissions(&bin_path).await?;
        
        tracing::info!("Extracted bundled MCP server '{}' to {:?}", entry.name, bin_path);
        
        Ok((Some(bin_path), true))
    }

    /// Set executable permissions on Unix systems
    #[cfg(unix)]
    async fn set_executable_permissions(path: &std::path::Path) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(path).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(path, perms).await?;
        Ok(())
    }

    /// Create MCP server configuration
    fn create_mcp_server_config(
        &self,
        entry: &McpManifestEntry,
        server_dir: &std::path::Path,
        binary_path: &Option<std::path::PathBuf>,
        from_bundle: bool,
    ) -> McpServerConfig {
        let command = if from_bundle {
            binary_path.as_ref().map(|p| p.to_string_lossy().to_string())
        } else {
            entry.command.clone()
        };

        McpServerConfig {
            name: entry.name.clone(),
            transport: parse_transport(&entry.transport),
            command,
            args: entry.args.clone(),
            env: entry.env.clone(),
            cwd: if from_bundle { Some(server_dir.to_path_buf()) } else { None },
            endpoint: None,
            auto_start: true,
            health_check_interval_secs: 30,
            max_restarts: 0,
            init_timeout_secs: 30,
            tool_timeout_secs: 60,
            reserved_parameters: HashMap::new(),
            bundle: from_bundle,
            bundled_path: binary_path.clone(),
        }
    }

    /// Install tools from registry references
    async fn install_tools_from_registry(
        &self,
        manifest: &AgentManifest,
    ) -> anyhow::Result<Vec<String>> {
        let mut installed = Vec::new();

        if manifest.tool_registry.required.is_empty() {
            return Ok(installed);
        }

        // TODO: Implement tool registry client installation
        // For now, just log the tools that would be installed
        for tool_ref in &manifest.tool_registry.required {
            tracing::info!(
                "Tool registry install: {} @ {} from {}",
                tool_ref.name,
                tool_ref.version,
                tool_ref.source
            );
            installed.push(tool_ref.name.clone());
        }

        tracing::warn!(
            "Tool registry installation not yet implemented. \
             {} tools referenced in manifest.",
            installed.len()
        );

        Ok(installed)
    }
}

/// Convenience function to import an agent
pub async fn import_agent(
    package_path: impl AsRef<Path>,
    options: ImportOptions,
) -> anyhow::Result<ImportResult> {
    let unpackager = Unpackager::new(package_path);
    unpackager.import(options).await
}

/// Convenience function to inspect a package
pub async fn inspect_agent(
    package_path: impl AsRef<Path>,
    passphrase: Option<&str>,
) -> anyhow::Result<(AgentManifest, ValidationResult)> {
    let unpackager = Unpackager::new(package_path);
    unpackager.inspect(passphrase).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_import_options_default() {
        let opts = ImportOptions::default();
        assert!(!opts.rotate_keys);
        #[allow(deprecated)]
        {
            assert!(opts.import_memory); // Deprecated but still true for backward compat
        }
        assert!(opts.import_mcp);
        assert!(!opts.install_tools_from_registry);
        assert!(!opts.skip_validation);
        assert!(!opts.force);
    }
}
