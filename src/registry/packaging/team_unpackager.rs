//! Team Unpackager for importing portable team packages
//!
//! Extracts and imports .team files into the local peko runtime

use crate::registry::packaging::team_packager::TeamManifest;
use crate::registry::packaging::{ImportOptions as AgentImportOptions, Unpackager};
use anyhow::Context;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// Team import options
#[derive(Debug, Clone)]
pub struct TeamImportOptions {
    /// New name for the imported team (optional)
    pub new_name: Option<String>,
    /// Import sessions
    pub import_sessions: bool,
    /// Import workspace
    pub import_workspace: bool,
    /// Import MCP servers
    pub import_mcp: bool,
    /// Rotate keys on import
    pub rotate_keys: bool,
    /// Force import even if team exists
    pub force: bool,
    /// Allow importing unsigned agent packages (issue #14).
    /// See [`crate::registry::packaging::ImportOptions::allow_unsigned`].
    pub allow_unsigned: bool,
}

impl Default for TeamImportOptions {
    fn default() -> Self {
        Self {
            new_name: None,
            import_sessions: true,
            import_workspace: true,
            import_mcp: true,
            rotate_keys: true,
            force: false,
            allow_unsigned: false,
        }
    }
}

/// Team import result
#[derive(Debug, Clone)]
pub struct TeamImportResult {
    /// Team name
    pub name: String,
    /// Number of agents imported
    pub agent_count: usize,
    /// Individual agent import results
    pub agents: Vec<AgentImportSummary>,
    /// Workspace path
    pub workspace_path: std::path::PathBuf,
}

/// Agent import summary
#[derive(Debug, Clone)]
pub struct AgentImportSummary {
    /// Agent name
    pub name: String,
    /// Agent DID
    pub did: String,
    /// Whether keys were rotated
    pub keys_rotated: bool,
}

/// Team unpackager for importing .team packages
pub struct TeamUnpackager {
    /// Package path
    package_path: std::path::PathBuf,
    /// Base directory for import
    base_dir: std::path::PathBuf,
}

impl TeamUnpackager {
    /// Create a new team unpackager
    pub fn new(package_path: impl AsRef<Path>) -> Self {
        Self {
            package_path: package_path.as_ref().to_path_buf(),
            base_dir: dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("peko"),
        }
    }

    /// Set custom base directory
    pub fn with_base_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.base_dir = dir.as_ref().to_path_buf();
        self
    }

    /// Inspect a team package without importing
    pub async fn inspect(&self) -> anyhow::Result<TeamManifest> {
        let files = self.extract_package().await?;

        let manifest_bytes = files
            .get("team/manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing team/manifest.toml in package"))?;
        let manifest_str = std::str::from_utf8(manifest_bytes)?;
        let manifest = TeamManifest::from_toml(manifest_str)?;

        Ok(manifest)
    }

    /// Import the team package
    pub async fn import(&self, options: TeamImportOptions) -> anyhow::Result<TeamImportResult> {
        let files = self.extract_package().await?;

        // Parse manifest
        let manifest = self.parse_manifest(&files)?;

        // Validate checksums if packaging metadata is present
        self.validate_checksums(&manifest, &files)?;

        let team_name = options
            .new_name
            .clone()
            .unwrap_or_else(|| manifest.team.name.clone());

        // Create team directory
        let team_dir = self.base_dir.join("teams").join(team_name.clone());
        if team_dir.exists() && !options.force {
            anyhow::bail!("Team '{team_name}' already exists. Use --force to overwrite.");
        }

        tokio::fs::create_dir_all(&team_dir)
            .await
            .with_context(|| format!("Failed to create team directory: {}", team_dir.display()))?;

        // Restore team.toml if present in package
        if let Some(team_toml_content) = files.get("team/team.toml") {
            let team_toml_path = team_dir.join("team.toml");
            tokio::fs::write(&team_toml_path, team_toml_content)
                .await
                .with_context(|| {
                    format!("Failed to write team.toml: {}", team_toml_path.display())
                })?;
        }

        // Group files by agent
        let agent_files = self.group_files_by_agent(&files);

        let mut imported_agents = Vec::new();

        // Import each agent
        for (agent_name, agent_data) in agent_files {
            let agent_result = self
                .import_agent_files(&agent_name, &agent_data, &team_name, &options)
                .await
                .with_context(|| format!("Failed to import agent: {agent_name}"))?;

            imported_agents.push(agent_result);
        }

        // Per ADR-031, workspaces are agent-scoped under workspaces/{agent}/{team}/.
        // There is no single team workspace directory; return the workspaces root.
        Ok(TeamImportResult {
            name: team_name.clone(),
            agent_count: imported_agents.len(),
            agents: imported_agents,
            workspace_path: self.base_dir.join("workspaces"),
        })
    }

    /// Validate checksums for all files in the package
    fn validate_checksums(
        &self,
        manifest: &TeamManifest,
        files: &HashMap<String, Vec<u8>>,
    ) -> anyhow::Result<()> {
        let packaging = match &manifest.packaging {
            Some(p) => p,
            None => {
                // No packaging metadata — warn but continue (legacy package)
                eprintln!("Warning: Team package has no packaging metadata (checksums). Skipping integrity validation.");
                return Ok(());
            }
        };

        for (path, expected_checksum) in &packaging.checksums {
            // Skip the manifest itself — it's validated by being parsed
            if path == "team/manifest.toml" {
                continue;
            }

            let content = files.get(path).ok_or_else(|| {
                anyhow::anyhow!("Package is missing file listed in packaging metadata: {path}")
            })?;

            let computed = Self::compute_checksum(content);
            if computed != *expected_checksum {
                anyhow::bail!(
                    "Checksum mismatch for '{path}': expected {expected_checksum}, got {computed}"
                );
            }
        }

        Ok(())
    }

    /// Compute SHA-256 checksum for data
    fn compute_checksum(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Extract package files
    async fn extract_package(&self) -> anyhow::Result<HashMap<String, Vec<u8>>> {
        let tar_gz = std::fs::File::open(&self.package_path)
            .with_context(|| format!("Failed to open package: {}", self.package_path.display()))?;
        let tar = flate2::read::GzDecoder::new(tar_gz);
        let mut archive = tar::Archive::new(tar);

        let mut files = HashMap::new();

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_string_lossy().to_string();

            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;

            files.insert(path, content);
        }

        Ok(files)
    }

    /// Parse manifest from files
    fn parse_manifest(&self, files: &HashMap<String, Vec<u8>>) -> anyhow::Result<TeamManifest> {
        let manifest_bytes = files
            .get("team/manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing team/manifest.toml in package"))?;
        let manifest_str = std::str::from_utf8(manifest_bytes)?;
        let manifest = TeamManifest::from_toml(manifest_str)?;

        Ok(manifest)
    }

    /// Group files by agent
    fn group_files_by_agent(
        &self,
        files: &HashMap<String, Vec<u8>>,
    ) -> HashMap<String, HashMap<String, Vec<u8>>> {
        let mut agents: HashMap<String, HashMap<String, Vec<u8>>> = HashMap::new();

        for (path, content) in files {
            if let Some(agent_path) = path.strip_prefix("agents/") {
                if let Some((agent_name, file_path)) = agent_path.split_once('/') {
                    agents
                        .entry(agent_name.to_string())
                        .or_default()
                        .insert(file_path.to_string(), content.clone());
                }
            }
        }

        agents
    }

    /// Import a single agent's files directly without creating temporary files
    async fn import_agent_files(
        &self,
        name: &str,
        files: &HashMap<String, Vec<u8>>,
        team_name: &str,
        options: &TeamImportOptions,
    ) -> anyhow::Result<AgentImportSummary> {
        // Clone files for this agent to pass to unpackager
        let agent_files = files.clone();

        // Use the regular Unpackager with in-memory files.
        // Agents are standalone per ADR-031, so import to the global agents directory.
        let unpackager = Unpackager::new("dummy.agent") // Path doesn't matter for in-memory import
            .with_base_dir(&self.base_dir)
            .with_team(team_name);

        let agent_opts = AgentImportOptions {
            new_name: Some(name.to_string()),
            passphrase: None,
            rotate_keys: options.rotate_keys,
            import_sessions: options.import_sessions,
            import_workspace: options.import_workspace,
            skip_validation: false,
            force: options.force,
            team: Some(team_name.to_string()),
            allow_unsigned: options.allow_unsigned,
        };

        let result = unpackager
            .import_from_files(agent_files, agent_opts)
            .await
            .with_context(|| format!("Failed to import agent: {name}"))?;

        Ok(AgentImportSummary {
            name: result.name,
            did: result.did,
            keys_rotated: result.keys_rotated,
        })
    }
}

/// Convenience function to import a team
pub async fn import_team(
    package_path: impl AsRef<Path>,
    options: TeamImportOptions,
) -> anyhow::Result<TeamImportResult> {
    let unpackager = TeamUnpackager::new(package_path);
    unpackager.import(options).await
}

/// Import a team with a custom base directory
pub async fn import_team_with_base_dir(
    package_path: impl AsRef<Path>,
    base_dir: impl AsRef<Path>,
    options: TeamImportOptions,
) -> anyhow::Result<TeamImportResult> {
    let unpackager = TeamUnpackager::new(package_path).with_base_dir(base_dir);
    unpackager.import(options).await
}

/// Inspect a team package without importing
pub async fn inspect_team(package_path: impl AsRef<Path>) -> anyhow::Result<TeamManifest> {
    let unpackager = TeamUnpackager::new(package_path);
    unpackager.inspect().await
}
