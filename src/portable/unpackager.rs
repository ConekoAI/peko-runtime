//! Unpackager for importing portable agent packages
//!
//! Extracts and imports .agent files into the local peko runtime
#![allow(dead_code)]

use crate::identity::{storage::KeyStorage, Identity, KeyPairExport};
use crate::portable::{
    crypto::{decrypt_with_passphrase, deserialize_encrypted},
    manifest::AgentManifest,
    signature::{self, SignatureStatus},
    validation::{validate_package, ValidationResult},
};
use crate::session::key::derive_base_session_key;
use crate::session::types::Peer;
use crate::types::agent::AgentConfig;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// Import options
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// New name for the imported agent (optional)
    pub new_name: Option<String>,
    /// Passphrase for decryption (if encrypted)
    pub passphrase: Option<String>,
    /// Rotate keys (generate new DID)
    pub rotate_keys: bool,

    /// Import session history
    pub import_sessions: bool,
    /// Import workspace files
    pub import_workspace: bool,
    /// Skip validation (not recommended)
    pub skip_validation: bool,
    /// Force import even if DID exists
    pub force: bool,
    /// Team name for workspace/sessions placement
    pub team: Option<String>,
    /// Allow importing an unsigned `.agent` package (issue #14).
    ///
    /// Default: `false`. When `false`, any package whose
    /// `signatures.manifest` is empty fails the import with
    /// `signature_verification_failed`. This is a security check and is
    /// *not* affected by `force` or `skip_validation`.
    pub allow_unsigned: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            new_name: None,
            passphrase: None,
            rotate_keys: false,

            import_sessions: true,  // Default: import if present
            import_workspace: true, // Default: import if present
            skip_validation: false,
            force: false,
            team: None,
            allow_unsigned: false,
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

    /// Path to imported workspace
    pub workspace_path: Option<std::path::PathBuf>,
    /// Path to imported sessions
    pub sessions_path: Option<std::path::PathBuf>,
    /// Path to agent config
    pub config_path: std::path::PathBuf,
    /// Validation result
    pub validation: ValidationResult,
}

/// Unpackager for importing .agent packages
pub struct Unpackager {
    /// Package file path
    package_path: std::path::PathBuf,
    /// Base directory for imported agents
    base_dir: std::path::PathBuf,
    /// Team name for workspace/sessions placement (None for standalone imports)
    team: Option<String>,
}

impl Unpackager {
    /// Create a new unpackager
    pub fn new(package_path: impl AsRef<Path>) -> Self {
        Self {
            package_path: package_path.as_ref().to_path_buf(),
            base_dir: dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("peko"),
            team: None,
        }
    }

    /// Set custom base directory
    pub fn with_base_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.base_dir = dir.as_ref().to_path_buf();
        self
    }

    /// Set team name for workspace/sessions placement
    pub fn with_team(mut self, team: impl Into<String>) -> Self {
        self.team = Some(team.into());
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
        // Parse manifest. We keep the raw bytes around because the
        // signature verification path needs them: parsing and
        // re-serializing would lose any byte-level changes an attacker
        // introduced in transit, and the signature is over the exact
        // bytes the packager wrote.
        let manifest_bytes = files
            .get("manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing manifest.toml"))?
            .clone();
        let manifest_str = std::str::from_utf8(&manifest_bytes)?;
        let manifest = AgentManifest::from_toml(manifest_str)?;

        // ── Signature check (issue #14) ──────────────────────────────
        // Signature verification is a security guarantee. It runs
        // unconditionally — `force` and `skip_validation` do NOT
        // bypass it. A user who genuinely wants to pull from a source
        // they don't fully trust can pass `--allow-unsigned-agent`
        // (mapped to `allow_unsigned`); that opts in to importing an
        // *unsigned* package only. A *badly signed* package is always
        // rejected.
        let did_doc_bytes = files
            .get("identity/did.json")
            .ok_or_else(|| anyhow::anyhow!("Missing identity/did.json"))?
            .clone();
        match signature::verify_manifest_signature(
            &manifest_bytes,
            &did_doc_bytes,
            options.allow_unsigned,
        ) {
            Ok(SignatureStatus::Verified) => {
                tracing::debug!(
                    "manifest signature verified for agent '{}'",
                    manifest.agent.name
                );
            }
            Ok(SignatureStatus::AllowedUnsigned) => {
                // Already logged inside verify_manifest_signature.
            }
            Err(e) => {
                // Stable CLI error code: signature_verification_failed
                // (issue #14 acceptance criteria). The human reason is
                // in `e`.
                return Err(anyhow::anyhow!(
                    "[signature_verification_failed] Manifest signature check failed: {e}"
                ));
            }
        }

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

        // Import skills
        self.import_skills(&files).await?;

        // Install embedded extensions from `extensions/` layer (ADR-037).
        // Failures are logged but do not break the agent import.
        self.import_embedded_extensions(&files).await?;

        // Import workspace (if present in package)
        if options.import_workspace {
            self.import_workspace(&files, &name).await?;
        }

        // Import sessions (if present in package)
        let sessions_imported = if options.import_sessions {
            self.import_sessions(&files, &name).await?;
            true
        } else {
            false
        };

        // Compute workspace and sessions paths for result
        let team = options.team.as_deref().unwrap_or("default");

        // After importing sessions, ensure the most recent session is active
        // for the default peer. This is critical for memory continuity when
        // the agent is imported with a different name (peer keys are based on
        // agent name, so the imported index won't match the new agent).
        if sessions_imported {
            if let Err(e) = self.activate_most_recent_session(&name, team).await {
                tracing::warn!(
                    "Failed to activate session for imported agent '{}': {}",
                    name,
                    e
                );
            }
        }

        // Save config
        let config_path = self.save_config(&config, &name).await?;

        // ADR-031 layout: workspaces/{agent}/{team}/
        let workspace_path = if options.import_workspace {
            Some(dirs::data_dir().map_or_else(
                || self.base_dir.join("workspaces").join(&name).join(team),
                |d| d.join("peko").join("workspaces").join(&name).join(team),
            ))
        } else {
            None
        };

        // ADR-031 layout: sessions/{agent}/{team}/
        let sessions_path = if options.import_sessions {
            Some(dirs::data_dir().map_or_else(
                || self.base_dir.join("sessions").join(&name).join(team),
                |d| d.join("peko").join("sessions").join(&name).join(team),
            ))
        } else {
            None
        };

        Ok(ImportResult {
            name,
            did: identity.did,
            keys_rotated: options.rotate_keys,
            workspace_path,
            sessions_path,
            config_path,
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
        identity: &Identity,
    ) -> anyhow::Result<AgentConfig> {
        let config_bytes = files
            .get("config/agent.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing config/agent.toml"))?;
        let config_str = std::str::from_utf8(config_bytes)?;
        let mut config: AgentConfig = toml::from_str(config_str)?;

        // Update runtime-specific fields. The `host_runtime_id`
        // and `owner_id` are derived from the .agent file's
        // `identity/did.json`, but `import_agent` runs with
        // `rotate_keys: true` (see agent_service.rs:782) which
        // generates a fresh DID locally and stores its key in
        // this daemon's `KeyStorage`. Without rebinding these
        // two fields to the new identity's DID, the chat path
        // would try to sign with the AUTHOR's key — which the
        // collab doesn't have — and the chat silently returns
        // empty (the `peko send --no-stream` path swallows the
        // missing-key error). Phase D3 flow 5 is the first
        // end-to-end test that exercises this code path and
        // surfaces the regression.
        config.name = new_name.to_string();
        config.host_runtime_id = identity.did.clone();
        // Owner is the bare `"local"` string to match what
        // `CallerContext::subject()` returns for an `Identity::Local`
        // caller — otherwise `check_permission` would never match
        // (cross-kind guard via `Principal` equality). The runtime DID
        // is still recorded in `host_runtime_id` above for audit
        // purposes. (ADR-039 follow-up; this was a pre-existing
        // mismatch surfaced by the audit pass.)
        config.owner = crate::auth::principal::Principal::User("local".to_string());

        // Note: Memory import is no longer supported as core memory has been deprecated.
        // The config.memory field no longer exists in AgentConfig.

        Ok(config)
    }

    /// Import skills from a legacy `skills/` layer.
    ///
    /// # Deprecation
    ///
    /// The `skills/` layer is legacy. New packages produced by ADR-037
    /// tooling use `AgentManifest.extensions` to declare skill/MCP
    /// dependencies, which are resolved and installed via the extension
    /// manager on pull. This method is kept for backward compatibility
    /// with older `.agent` files that still contain a `skills/` layer.
    async fn import_skills(&self, files: &HashMap<String, Vec<u8>>) -> anyhow::Result<()> {
        let skills_dir = dirs::data_dir()
            .map(|d| d.join("peko").join("skills"))
            .unwrap_or_else(|| self.base_dir.join("skills"));
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

    /// Install embedded extension packages from the `extensions/` layer.
    ///
    /// Each file matching `extensions/{id}.ext` is written to a temporary
    /// `.ext` file and installed through the extension manager. Failures
    /// for individual extensions are logged but do not fail the overall
    /// agent import.
    async fn import_embedded_extensions(
        &self,
        files: &HashMap<String, Vec<u8>>,
    ) -> anyhow::Result<()> {
        use crate::extension::manager::{
            packaging::ExtensionUnpackager, ExtensionManager,
        };
        use crate::extensions::builtin::{BuiltinToolAdapter, BuiltinToolRegistrarConfig};
        use crate::extensions::gateway::GatewayAdapter;
        use crate::extensions::general::GeneralExtensionAdapter;
        use crate::extensions::mcp::McpAdapter;
        use crate::extensions::skill::SkillAdapter;
        use crate::extensions::universal::UniversalToolAdapter;

        let extensions_dir = dirs::data_dir()
            .map(|d| d.join("peko").join("extensions"))
            .unwrap_or_else(|| self.base_dir.join("extensions"));
        tokio::fs::create_dir_all(&extensions_dir).await?;

        for (path, content) in files {
            let Some(ext_file) = path.strip_prefix("extensions/") else {
                continue;
            };
            // Only process .ext files directly under extensions/
            if !ext_file.ends_with(".ext") || ext_file.contains('/') {
                continue;
            }

            let temp_dir = std::env::temp_dir().join(format!(
                "peko-agent-embed-{}-{}",
                ext_file,
                std::process::id()
            ));
            if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
                tracing::warn!(
                    "Failed to create temp dir for embedded extension '{}': {}",
                    ext_file,
                    e
                );
                continue;
            }

            let temp_ext_path = temp_dir.join(ext_file);
            if let Err(e) = tokio::fs::write(&temp_ext_path, content).await {
                tracing::warn!(
                    "Failed to write embedded extension '{}': {}",
                    ext_file,
                    e
                );
                let _ = tokio::fs::remove_dir_all(&temp_dir).await;
                continue;
            }

            // Extract the .ext package to a temp directory, then install it
            let extract_dir = temp_dir.join("extracted");
            let install_result = tokio::task::spawn_blocking({
                let extract_dir = extract_dir.clone();
                let temp_ext_path = temp_ext_path.clone();
                move || ExtensionUnpackager::install(&temp_ext_path, &extract_dir)
            })
            .await;

            match install_result {
                Ok(Ok(extracted_path)) => {
                    let mut manager = ExtensionManager::new().with_storage_dir(extensions_dir.clone());
                    let _ = BuiltinToolAdapter::register_all(
                        &manager.core_arc(),
                        &BuiltinToolRegistrarConfig::default(),
                    )
                    .await;
                    manager.register_adapter(Box::new(SkillAdapter::new()));
                    manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
                    manager.register_adapter(Box::new(UniversalToolAdapter::new()));
                    manager.register_adapter(Box::new(GatewayAdapter::new(manager.core_arc())));
                    manager.register_adapter(Box::new(GeneralExtensionAdapter::new()));
                    if let Err(e) = manager.install(&extracted_path).await {
                        tracing::warn!(
                            "Failed to install embedded extension '{}': {}",
                            ext_file,
                            e
                        );
                    } else {
                        tracing::info!(
                            "Installed embedded extension '{}' from agent package",
                            ext_file
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        "Failed to extract embedded extension '{}': {}",
                        ext_file,
                        e
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Panic while extracting embedded extension '{}': {}",
                        ext_file,
                        e
                    );
                }
            }

            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        }

        Ok(())
    }

    /// Import workspace files
    async fn import_workspace(
        &self,
        files: &HashMap<String, Vec<u8>>,
        agent_name: &str,
    ) -> anyhow::Result<()> {
        // ADR-031 layout: workspaces/{agent}/{team}/
        let workspace_dir = dirs::data_dir()
            .map(|d| {
                d.join("peko")
                    .join("workspaces")
                    .join(agent_name)
                    .join(self.team.as_deref().unwrap_or("default"))
            })
            .unwrap_or_else(|| {
                self.base_dir
                    .join("workspaces")
                    .join(agent_name)
                    .join(self.team.as_deref().unwrap_or("default"))
            });

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
    async fn import_sessions(
        &self,
        files: &HashMap<String, Vec<u8>>,
        agent_name: &str,
    ) -> anyhow::Result<()> {
        // ADR-031 layout: sessions/{agent}/{team}/
        let sessions_dir = dirs::data_dir()
            .map(|d| {
                d.join("peko")
                    .join("sessions")
                    .join(agent_name)
                    .join(self.team.as_deref().unwrap_or("default"))
            })
            .unwrap_or_else(|| {
                self.base_dir
                    .join("sessions")
                    .join(agent_name)
                    .join(self.team.as_deref().unwrap_or("default"))
            });

        tokio::fs::create_dir_all(&sessions_dir).await?;

        // Get the original agent name from manifest to know what to replace
        let original_agent_name = self
            .parse_manifest(files)
            .ok()
            .map(|m| m.agent.name)
            .unwrap_or_default();

        for (path, content) in files {
            if path.starts_with("sessions/") {
                let file_name = path.strip_prefix("sessions/").unwrap_or(path);
                let dest_path = sessions_dir.join(file_name);

                if let Some(parent) = dest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                // If importing with a different name, update sessions.json agent_name fields
                let final_content = if file_name == "sessions.json"
                    && !original_agent_name.is_empty()
                    && original_agent_name != agent_name
                {
                    if let Ok(mut sessions) = serde_json::from_slice::<
                        std::collections::HashMap<String, serde_json::Value>,
                    >(content)
                    {
                        for (_, session_val) in sessions.iter_mut() {
                            if let Some(obj) = session_val.as_object_mut() {
                                if let Some(agent_name_val) = obj.get_mut("agent_name") {
                                    *agent_name_val =
                                        serde_json::Value::String(agent_name.to_string());
                                }
                            }
                        }
                        serde_json::to_vec_pretty(&sessions).unwrap_or_else(|_| content.clone())
                    } else {
                        content.clone()
                    }
                } else {
                    content.clone()
                };

                tokio::fs::write(dest_path, final_content).await?;
            }
        }

        Ok(())
    }

    /// Activate the most recent imported session for the default peer.
    /// This ensures memory continuity works immediately after import.
    async fn activate_most_recent_session(
        &self,
        agent_name: &str,
        team: &str,
    ) -> anyhow::Result<()> {
        // ADR-031 layout: sessions/{agent}/{team}/
        let sessions_dir = dirs::data_dir()
            .map(|d| d.join("peko").join("sessions").join(agent_name).join(team))
            .unwrap_or_else(|| self.base_dir.join("sessions").join(agent_name).join(team));

        if !sessions_dir.exists() {
            return Ok(());
        }

        let mut controller =
            crate::session::metadata_controller::MetadataController::new(&sessions_dir);

        // List all sessions and find the most recent one
        let entries = controller.list_all_from_index().await?;
        let most_recent = entries.into_iter().max_by_key(|e| e.updated_at);

        if let Some(entry) = most_recent {
            let peer = Peer::User("default".to_string());
            let peer_key = derive_base_session_key(agent_name, &peer);
            controller
                .ensure_peer_active(&peer_key, &entry.session_id)
                .await?;
            tracing::info!(
                "Activated session {} for imported agent '{}' (peer_key: {})",
                entry.session_id,
                agent_name,
                peer_key
            );
        }

        Ok(())
    }

    /// Save agent config
    async fn save_config(
        &self,
        config: &AgentConfig,
        name: &str,
    ) -> anyhow::Result<std::path::PathBuf> {
        // Nest the agent under its team directory when this unpackager was
        // constructed via `with_team(...)` (the team import path).
        // Standalone agent imports leave `self.team = None` and continue
        // to use the flat `agents/<name>/` layout, matching the pre-team
        // behaviour. Without this, a team import would write the agent
        // config next to unrelated agents instead of under the team the
        // manifest claims it belongs to.
        let agent_dir = if let Some(team) = self.team.as_deref() {
            self.base_dir
                .join("teams")
                .join(team)
                .join("agents")
                .join(name)
        } else {
            self.base_dir.join("agents").join(name)
        };
        tokio::fs::create_dir_all(&agent_dir).await?;

        let config_path = agent_dir.join("config.toml");
        let config_toml = toml::to_string_pretty(config)?;

        tokio::fs::write(&config_path, config_toml).await?;

        Ok(config_path)
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
        assert!(!opts.skip_validation);
        assert!(!opts.force);
    }
}
