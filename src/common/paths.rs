//! Common path resolution utilities
//!
//! This module provides standardized path resolution for Pekobot's
//! team-aware directory structure. All components (CLI, API, daemon)
//! should use these utilities for consistent path resolution.
//!
//! # Directory Structure
//!
//! ## Config Directory (`{config_dir}`, e.g., `~/.pekobot`)
//! ```text
//! ~/.pekobot/                          # Config root
//! └── teams/
//!     └── {team}/
//!         └── agents/
//!             └── {agent}/
//!                 ├── config.toml      # Agent configuration
//!                 ├── tools/           # Agent-specific tools
//!                 └── skills/          # Agent-specific skills
//! ```
//!
//! ## Data Directory (`{data_dir}`, e.g., `~/.local/share/pekobot`)
//! ```text
//! {data_dir}/
//! ├── tools/                           # Downloaded/installed tools
//! ├── cron.db                         # Cron job database
//! ├── sessions/                       # Session history (*.jsonl)
//! │   └── {team}/
//! │       └── {agent}/
//! │           └── {session_id}.jsonl
//! └── workspaces/                     # Agent workspace files
//!     └── {team}/
//!         └── {agent}/
//!             ├── AGENTS.md
//!             ├── SOUL.md
//!             └── ...
//! ```

use std::path::{Path, PathBuf};

/// Default team name when none is specified
pub const DEFAULT_TEAM: &str = "default";

/// Get the default configuration directory
///
/// Returns `~/.pekobot` or the current directory's `.pekobot` folder
/// as a fallback if home directory is not available.
#[must_use] 
pub fn default_config_dir() -> PathBuf {
    dirs::home_dir().map_or_else(|| PathBuf::from(".").join(".pekobot"), |d| d.join(".pekobot"))
}

/// Get the default data directory
///
/// Returns `~/.local/share/pekobot` on Linux,
/// `~/Library/Application Support/pekobot` on macOS,
/// or `%APPDATA%/pekobot` on Windows.
/// Falls back to the config directory if `data_dir` is not available.
pub fn default_data_dir() -> PathBuf {
    dirs::data_dir().map_or_else(default_config_dir, |d| d.join("pekobot"))
}

/// Get the default cache directory
///
/// Falls back to `{data_dir}/cache` if `cache_dir` is not available.
#[must_use] 
pub fn default_cache_dir() -> PathBuf {
    dirs::cache_dir().map_or_else(|| default_data_dir().join("cache"), |d| d.join("pekobot"))
}

/// Path resolver for Pekobot's directory structure
///
/// This struct provides methods to resolve paths for agents, teams,
/// sessions, and workspaces. It uses the configured base directories
/// and applies the team-aware path structure consistently.
///
/// # Path Categories
///
/// - **Config paths** (`config_dir`): Agent configurations, team metadata, credentials
/// - **Data paths** (`data_dir`): Sessions, workspaces, tools, cron database
/// - **Cache paths** (`cache_dir`): Temporary files, downloaded caches
#[derive(Debug, Clone)]
pub struct PathResolver {
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self {
            config_dir: default_config_dir(),
            data_dir: default_data_dir(),
            cache_dir: default_cache_dir(),
        }
    }
}

impl PathResolver {
    /// Create a new path resolver with default directories
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a path resolver with custom directories
    #[must_use] 
    pub fn with_dirs(config_dir: PathBuf, data_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            config_dir,
            data_dir,
            cache_dir,
        }
    }

    /// Create a path resolver from optional overrides
    ///
    /// Uses the provided overrides if available, otherwise uses defaults.
    pub fn from_overrides(
        config_dir: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            config_dir: config_dir.unwrap_or_else(default_config_dir),
            data_dir: data_dir.unwrap_or_else(default_data_dir),
            cache_dir: cache_dir.unwrap_or_else(default_cache_dir),
        }
    }

    /// Get the config directory
    #[must_use] 
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Get the data directory
    #[must_use] 
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Get the cache directory
    #[must_use] 
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    // ====================================================================================
    // Config Directory Paths (configuration, metadata)
    // ====================================================================================

    /// Get the teams configuration directory
    ///
    /// Path: `{config_dir}/teams`
    #[must_use] 
    pub fn teams_dir(&self) -> PathBuf {
        self.config_dir.join("teams")
    }

    /// Get a specific team's configuration directory
    ///
    /// Path: `{config_dir}/teams/{team}`
    #[must_use] 
    pub fn team_dir(&self, team: &str) -> PathBuf {
        self.teams_dir().join(team)
    }

    /// Get the agents directory for a specific team
    ///
    /// Path: `{config_dir}/teams/{team}/agents`
    #[must_use] 
    pub fn agents_dir(&self, team: Option<&str>) -> PathBuf {
        let team = team.unwrap_or(DEFAULT_TEAM);
        self.team_dir(team).join("agents")
    }

    /// Get the path to an agent's configuration file
    ///
    /// Path: `{config_dir}/teams/{team}/agents/{agent}/config.toml`
    #[must_use] 
    pub fn agent_config(&self, agent: &str, team: Option<&str>) -> PathBuf {
        self.agents_dir(team).join(agent).join("config.toml")
    }

    /// Get the MCP configuration file path
    ///
    /// Path: `{config_dir}/mcp.toml`
    #[must_use] 
    pub fn mcp_config(&self) -> PathBuf {
        self.config_dir.join("mcp.toml")
    }

    /// Get the Universal Tools directory
    ///
    /// Path: `{data_dir}/tools`
    #[must_use] 
    pub fn universal_tools_dir(&self) -> PathBuf {
        self.data_dir.join("tools")
    }

    /// Get the Skills directory
    ///
    /// Path: `{data_dir}/skills`
    #[must_use] 
    pub fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }

    // ====================================================================================
    // Data Directory Paths (sessions, workspaces, runtime data)
    // ====================================================================================

    /// Get the sessions root directory
    ///
    /// Path: `{data_dir}/sessions`
    #[must_use] 
    pub fn sessions_root(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    /// Get the sessions directory for a specific team
    ///
    /// Path: `{data_dir}/sessions/{team}`
    #[must_use] 
    pub fn team_sessions_dir(&self, team: &str) -> PathBuf {
        self.sessions_root().join(team)
    }

    /// Get the path to an agent's sessions directory
    ///
    /// Path: `{data_dir}/sessions/{team}/{agent}`
    #[must_use] 
    pub fn agent_sessions_dir(&self, agent: &str, team: Option<&str>) -> PathBuf {
        let team = team.unwrap_or(DEFAULT_TEAM);
        self.team_sessions_dir(team).join(agent)
    }

    /// Get the path to an agent's session file
    ///
    /// Path: `{data_dir}/sessions/{team}/{agent}/{session_id}.jsonl`
    #[must_use] 
    pub fn agent_session_file(&self, agent: &str, team: Option<&str>, session_id: &str) -> PathBuf {
        self.agent_sessions_dir(agent, team)
            .join(format!("{session_id}.jsonl"))
    }

    /// Get the workspaces root directory
    ///
    /// Path: `{data_dir}/workspaces`
    #[must_use] 
    pub fn workspaces_root(&self) -> PathBuf {
        self.data_dir.join("workspaces")
    }

    /// Get the workspace directory for a specific team
    ///
    /// Path: `{data_dir}/workspaces/{team}`
    #[must_use] 
    pub fn team_workspaces_dir(&self, team: &str) -> PathBuf {
        self.workspaces_root().join(team)
    }

    /// Get the path to an agent's workspace directory
    ///
    /// Path: `{data_dir}/workspaces/{team}/{agent}`
    #[must_use] 
    pub fn agent_workspace(&self, agent: &str, team: Option<&str>) -> PathBuf {
        let team = team.unwrap_or(DEFAULT_TEAM);
        self.team_workspaces_dir(team).join(agent)
    }

    /// Get the path to an agent's workspace directory with explicit team
    ///
    /// Same as `agent_workspace` but requires an explicit team.
    /// Path: `{data_dir}/workspaces/{team}/{agent}`
    #[must_use] 
    pub fn agent_workspace_with_team(&self, agent: &str, team: &str) -> PathBuf {
        self.team_workspaces_dir(team).join(agent)
    }

    /// Get the tools directory
    ///
    /// Path: `{data_dir}/tools`
    #[must_use] 
    pub fn tools_dir(&self) -> PathBuf {
        self.data_dir.join("tools")
    }

    /// Get the async tasks directory
    ///
    /// Path: `{data_dir}/async_tasks`
    #[must_use]
    pub fn async_tasks_dir(&self) -> PathBuf {
        self.data_dir.join("async_tasks")
    }

    // ====================================================================================
    // Utility Methods
    // ====================================================================================

    /// Ensure all base directories exist
    ///
    /// Creates directories if they don't exist. Returns Ok(()) if successful
    /// or if directories already exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        Ok(())
    }

    /// Ensure an agent's config directories exist
    ///
    /// Creates the agent's config directory.
    pub fn ensure_agent_config_dirs(&self, agent: &str, team: Option<&str>) -> std::io::Result<()> {
        std::fs::create_dir_all(self.agents_dir(team).join(agent))?;
        Ok(())
    }

    /// Ensure an agent's data directories exist
    ///
    /// Creates the agent's sessions and workspace directories.
    pub fn ensure_agent_data_dirs(&self, agent: &str, team: Option<&str>) -> std::io::Result<()> {
        std::fs::create_dir_all(self.agent_sessions_dir(agent, team))?;
        std::fs::create_dir_all(self.agent_workspace(agent, team))?;
        Ok(())
    }

    /// Ensure all directories for an agent exist (both config and data)
    pub fn ensure_agent_dirs(&self, agent: &str, team: Option<&str>) -> std::io::Result<()> {
        self.ensure_agent_config_dirs(agent, team)?;
        self.ensure_agent_data_dirs(agent, team)?;
        Ok(())
    }
}

/// Resolve team and agent from an identifier string
///
/// Supports formats:
/// - `team/agent` - Returns (team, agent)
/// - `agent` - Returns ("default", agent)
///
/// # Errors
/// Returns an error if the identifier format is invalid.
pub fn resolve_team_agent(identifier: &str) -> anyhow::Result<(String, String)> {
    if identifier.contains('/') {
        let parts: Vec<&str> = identifier.splitn(2, '/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(anyhow::anyhow!(
                "Invalid identifier format: '{identifier}'. Expected 'team/agent'"
            ));
        }
        Ok((parts[0].to_string(), parts[1].to_string()))
    } else {
        Ok((DEFAULT_TEAM.to_string(), identifier.to_string()))
    }
}

/// Resolve team and agent with an optional team override
///
/// The override takes precedence over the identifier.
/// Supports the same identifier formats as `resolve_team_agent`.
pub fn resolve_team_agent_with_override(
    identifier: &str,
    team_override: Option<&str>,
) -> anyhow::Result<(String, String)> {
    let (team_from_id, agent) = resolve_team_agent(identifier)?;
    let team = team_override.map_or(team_from_id, String::from);
    Ok((team, agent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_team_agent_simple() {
        let (team, agent) = resolve_team_agent("myagent").unwrap();
        assert_eq!(team, "default");
        assert_eq!(agent, "myagent");
    }

    #[test]
    fn test_resolve_team_agent_with_team() {
        let (team, agent) = resolve_team_agent("myteam/myagent").unwrap();
        assert_eq!(team, "myteam");
        assert_eq!(agent, "myagent");
    }

    #[test]
    fn test_resolve_team_agent_invalid() {
        assert!(resolve_team_agent("/agent").is_err());
        assert!(resolve_team_agent("team/").is_err());
    }

    #[test]
    fn test_resolve_with_override() {
        let (team, agent) =
            resolve_team_agent_with_override("myteam/myagent", Some("otherteam")).unwrap();
        assert_eq!(team, "otherteam");
        assert_eq!(agent, "myagent");
    }

    #[test]
    fn test_path_resolver_default() {
        let resolver = PathResolver::new();
        assert!(resolver.config_dir().to_string_lossy().contains(".pekobot"));
    }

    #[test]
    fn test_config_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        // Config paths should be in config_dir
        assert_eq!(
            resolver.agent_config("myagent", None),
            PathBuf::from("/config/teams/default/agents/myagent/config.toml")
        );

        assert_eq!(
            resolver.agent_config("myagent", Some("myteam")),
            PathBuf::from("/config/teams/myteam/agents/myagent/config.toml")
        );

        assert_eq!(resolver.teams_dir(), PathBuf::from("/config/teams"));
    }

    #[test]
    fn test_data_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        // Data paths should be in data_dir
        assert_eq!(
            resolver.agent_sessions_dir("myagent", Some("myteam")),
            PathBuf::from("/data/sessions/myteam/myagent")
        );

        assert_eq!(
            resolver.agent_session_file("myagent", Some("myteam"), "sess_123"),
            PathBuf::from("/data/sessions/myteam/myagent/sess_123.jsonl")
        );

        assert_eq!(
            resolver.agent_workspace("myagent", Some("myteam")),
            PathBuf::from("/data/workspaces/myteam/myagent")
        );

        assert_eq!(
            resolver.agent_workspace_with_team("myagent", "myteam"),
            PathBuf::from("/data/workspaces/myteam/myagent")
        );

        assert_eq!(resolver.tools_dir(), PathBuf::from("/data/tools"));

        assert_eq!(resolver.sessions_root(), PathBuf::from("/data/sessions"));

        assert_eq!(
            resolver.workspaces_root(),
            PathBuf::from("/data/workspaces")
        );
    }
}
