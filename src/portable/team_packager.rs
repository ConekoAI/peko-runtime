//! Team Packager for creating portable team packages
//!
//! Exports teams to `.team` files (tar.gz archives containing multiple agents)

use crate::identity::Identity;
use crate::portable::{ExportOptions as AgentExportOptions, Packager};
use crate::types::agent::AgentConfig;
use anyhow::Context;
use std::collections::HashMap;
use std::path::Path;

/// Team export options
#[derive(Debug, Clone)]
pub struct TeamExportOptions {
    /// Output path for the .team file
    pub output_path: Option<String>,
    /// Include sessions in export
    pub include_sessions: bool,
    /// Include workspace files
    pub include_workspace: bool,
    /// Include MCP servers
    pub include_mcp: bool,
    /// Description for the package
    pub description: Option<String>,
}

impl Default for TeamExportOptions {
    fn default() -> Self {
        Self {
            output_path: None,
            include_sessions: true,
            include_workspace: true,
            include_mcp: true,
            description: None,
        }
    }
}

/// Team packager for creating .team packages
pub struct TeamPackager {
    /// Team name
    team_name: String,
    /// Team description
    team_description: Option<String>,
    /// Agents to package (name, config, identity)
    agents: Vec<(String, AgentConfig, Identity)>,
    /// Skills directory
    skills_dir: Option<std::path::PathBuf>,
    /// Base directory for agent data
    base_dir: std::path::PathBuf,
}

/// Agent export data within a team package
type AgentExportData = (String, AgentConfig, Identity, HashMap<String, Vec<u8>>);

impl TeamPackager {
    /// Create a new team packager
    pub fn new(
        team_name: impl Into<String>,
        team_description: Option<String>,
        base_dir: impl AsRef<Path>,
    ) -> Self {
        Self {
            team_name: team_name.into(),
            team_description,
            agents: Vec::new(),
            skills_dir: None,
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    /// Add an agent to the team package
    pub fn add_agent(&mut self, name: impl Into<String>, config: AgentConfig, identity: Identity) {
        self.agents.push((name.into(), config, identity));
    }

    /// Set skills directory
    pub fn with_skills_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.skills_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Export the team to a .team package
    pub async fn export(&self, options: TeamExportOptions) -> anyhow::Result<std::path::PathBuf> {
        // Export each agent to get their files
        let mut agent_packages: Vec<AgentExportData> = Vec::new();

        for (name, config, identity) in &self.agents {
            let agent_files = self
                .export_agent_files(name, config, identity, &options)
                .await
                .with_context(|| format!("Failed to export agent: {}", name))?;
            agent_packages.push((name.clone(), config.clone(), identity.clone(), agent_files));
        }

        // Create the team package
        let output_path = self
            .create_team_archive(&agent_packages, &options)
            .await
            .context("Failed to create team archive")?;

        Ok(output_path)
    }

    /// Export a single agent's files
    async fn export_agent_files(
        &self,
        name: &str,
        config: &AgentConfig,
        identity: &Identity,
        options: &TeamExportOptions,
    ) -> anyhow::Result<HashMap<String, Vec<u8>>> {
        let agent_opts = AgentExportOptions {
            encrypt: false,
            passphrase: None,
            include_sessions: options.include_sessions,
            include_workspace: options.include_workspace,
            include_mcp: options.include_mcp,
            include_tool_registry: true,
            rotate_keys: false,
            description: None,
            output_path: None,
            mcp_config_path: None,
            tools_dir: None,
        };

        // Create a temporary packager for this agent
        let packager = Packager::new(config.clone(), identity.clone(), None);

        // Set up agent-specific paths
        let packager = if let Some(ref skills_dir) = self.skills_dir {
            packager.with_skills_dir(skills_dir)
        } else {
            packager
        };

        let workspace_dir = self
            .base_dir
            .join("workspaces")
            .join(&self.team_name)
            .join(name);
        let sessions_dir = self
            .base_dir
            .join("sessions")
            .join(&self.team_name)
            .join(name);

        let packager = packager
            .with_workspace_dir(&workspace_dir)
            .with_sessions_dir(&sessions_dir);

        // Collect files without creating archive
        let (files, _manifest) = packager
            .collect_files(agent_opts)
            .await
            .with_context(|| format!("Failed to collect files for agent: {}", name))?;

        Ok(files)
    }

    /// Create the final team archive
    async fn create_team_archive(
        &self,
        agent_packages: &[AgentExportData],
        options: &TeamExportOptions,
    ) -> anyhow::Result<std::path::PathBuf> {
        // Determine output path
        let output_path = if let Some(path) = &options.output_path {
            std::path::PathBuf::from(path)
        } else {
            std::path::PathBuf::from(format!("{}.team", self.team_name))
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
        let tar_gz = std::fs::File::create(&output_path)
            .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Add team manifest
        let team_manifest = self.create_team_manifest(agent_packages.len(), options);
        let manifest_toml =
            toml::to_string_pretty(&team_manifest).context("Failed to serialize team manifest")?;

        let mut header = tar::Header::new_gnu();
        header.set_path("team/manifest.toml")?;
        header.set_size(manifest_toml.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, manifest_toml.as_bytes())?;

        // Add each agent as a subdirectory
        for (name, _config, _identity, files) in agent_packages {
            for (file_path, content) in files {
                let tar_path = format!("agents/{}/{}", name, file_path);

                let mut header = tar::Header::new_gnu();
                header.set_path(&tar_path)?;
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();

                tar.append(&header, content.as_slice())
                    .with_context(|| format!("Failed to add file: {}", tar_path))?;
            }
        }

        // Finish archive
        tar.finish().context("Failed to finalize team archive")?;

        Ok(output_path)
    }

    /// Create team manifest
    fn create_team_manifest(
        &self,
        agent_count: usize,
        options: &TeamExportOptions,
    ) -> TeamManifest {
        TeamManifest {
            team: TeamInfo {
                name: self.team_name.clone(),
                description: options
                    .description
                    .clone()
                    .or(self.team_description.clone()),
                version: "1.0.0".to_string(),
                agent_count,
            },
            format: TeamFormat {
                version: "1.0".to_string(),
                pekobot_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            export: ExportMetadata {
                created_at: chrono::Utc::now().to_rfc3339(),
                include_sessions: options.include_sessions,
                include_workspace: options.include_workspace,
                include_mcp: options.include_mcp,
            },
        }
    }
}

/// Team manifest structure
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeamManifest {
    /// Team information
    pub team: TeamInfo,
    /// Format information
    pub format: TeamFormat,
    /// Export metadata
    pub export: ExportMetadata,
}

/// Team information
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeamInfo {
    /// Team name
    pub name: String,
    /// Team description
    pub description: Option<String>,
    /// Package version
    pub version: String,
    /// Number of agents in package
    pub agent_count: usize,
}

/// Format information
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeamFormat {
    /// Format version
    pub version: String,
    /// Pekobot version
    pub pekobot_version: String,
}

/// Export metadata
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportMetadata {
    /// Creation timestamp
    pub created_at: String,
    /// Whether sessions were included
    pub include_sessions: bool,
    /// Whether workspace was included
    pub include_workspace: bool,
    /// Whether MCP was included
    pub include_mcp: bool,
}

impl TeamManifest {
    /// Serialize to TOML
    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Parse from TOML
    pub fn from_toml(s: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(s)?)
    }
}

/// Convenience function to export a team
pub async fn export_team(
    team_name: impl Into<String>,
    team_description: Option<String>,
    base_dir: impl AsRef<Path>,
    agents: Vec<(String, AgentConfig, Identity)>,
    options: TeamExportOptions,
) -> anyhow::Result<std::path::PathBuf> {
    let mut packager = TeamPackager::new(team_name, team_description, base_dir);

    for (name, config, identity) in agents {
        packager.add_agent(name, config, identity);
    }

    packager.export(options).await
}
