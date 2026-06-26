//! Common path resolution utilities
//!
//! This module provides standardized path resolution for Peko's
//! directory structure. All components (CLI, API, daemon) should use
//! these utilities for consistent path resolution.
//!
//! # Directory Structure (ADR-031)
//!
//! ## Config Directory (`{config_dir}`, e.g., `~/.peko`)
//! ```text
//! ~/.peko/                          # Config root
//! ├── agents/                       # Top-level agent storage
//! │   └── {agent}/
//! │       ├── config.toml           # Agent configuration
//! │       ├── memberships.toml      # Team memberships
//! │       ├── tools/                # Agent-specific tools
//! │       └── skills/               # Agent-specific skills
//! └── teams/
//!     └── {team}/
//!         ├── team.toml             # Team metadata
//!         ├── members.toml          # Agent references + roles
//!         ├── extensions.toml       # Team-wide extensions
//!         └── shared/               # Shared files
//! ```
//!
//! ## Data Directory (`{data_dir}`, e.g., `~/.local/share/peko`)
//! ```text
//! {data_dir}/
//! ├── tools/                        # Downloaded/installed tools
//! ├── cron.json                     # Cron job database
//! ├── sessions/                     # Session history (*.jsonl)
//! │   └── {agent}/                  # Agent-scoped sessions
//! │       ├── personal/             # Standalone sessions
//! │       └── {team}/               # Team-context sessions
//! └── workspaces/                   # Agent workspace files
//!     └── {agent}/
//!         ├── personal/             # Standalone workspace
//!         └── {team}/               # Team-context workspace
//! ```

use std::path::{Path, PathBuf};

/// Environment variable override for Peko's home directory.
const PEKO_HOME_ENV: &str = "PEKO_HOME";

/// Get the default configuration directory
///
/// Checks `PEKO_HOME` environment variable first, then falls back to
/// `~/.peko` or the current directory's `.peko` folder.
#[must_use]
pub fn default_config_dir() -> PathBuf {
    if let Ok(peko_home) = std::env::var(PEKO_HOME_ENV) {
        return PathBuf::from(peko_home);
    }
    dirs::home_dir().map_or_else(|| PathBuf::from(".").join(".peko"), |d| d.join(".peko"))
}

/// Get the default data directory
///
/// Checks `PEKO_HOME` environment variable first, then falls back to
/// `~/.local/share/peko` on Linux,
/// `~/Library/Application Support/peko` on macOS,
/// or `%APPDATA%/peko` on Windows.
/// Falls back to the config directory if `data_dir` is not available.
pub fn default_data_dir() -> PathBuf {
    if let Ok(peko_home) = std::env::var(PEKO_HOME_ENV) {
        return PathBuf::from(peko_home).join("data");
    }
    dirs::data_dir().map_or_else(default_config_dir, |d| d.join("peko"))
}

/// Get the default cache directory
///
/// Checks `PEKO_HOME` environment variable first, then falls back to
/// platform cache directory. Falls back to `{data_dir}/cache` if
/// `cache_dir` is not available.
#[must_use]
pub fn default_cache_dir() -> PathBuf {
    if let Ok(peko_home) = std::env::var(PEKO_HOME_ENV) {
        return PathBuf::from(peko_home).join("cache");
    }
    dirs::cache_dir().map_or_else(|| default_data_dir().join("cache"), |d| d.join("peko"))
}

/// Path resolver for Peko's directory structure
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

    /// Get the top-level agents directory
    ///
    /// Path: `{config_dir}/agents`
    #[must_use]
    pub fn agents_root_dir(&self) -> PathBuf {
        self.config_dir.join("agents")
    }

    /// Get a specific agent's directory
    ///
    /// Path: `{config_dir}/agents/{agent}`
    #[must_use]
    pub fn agent_dir(&self, agent: &str) -> PathBuf {
        self.agents_root_dir().join(agent)
    }

    /// Get the path to an agent's config file
    ///
    /// Path: `{config_dir}/agents/{agent}/config.toml`
    #[must_use]
    pub fn agent_config(&self, agent: &str) -> PathBuf {
        self.agent_dir(agent).join("config.toml")
    }

    /// Get the path to an agent's memberships file
    ///
    /// Path: `{config_dir}/agents/{agent}/memberships.toml`
    #[must_use]
    pub fn agent_memberships(&self, agent: &str) -> PathBuf {
        self.agent_dir(agent).join("memberships.toml")
    }

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

    /// Get the team members file path
    ///
    /// Path: `{config_dir}/teams/{team}/members.toml`
    #[must_use]
    pub fn team_members(&self, team: &str) -> PathBuf {
        self.team_dir(team).join("members.toml")
    }

    /// Check if an agent exists
    #[must_use]
    pub fn agent_exists(&self, agent: &str) -> bool {
        self.agent_config(agent).exists()
    }

    /// Get the MCP configuration file path
    ///
    /// Path: `{config_dir}/mcp.toml`
    #[must_use]
    pub fn mcp_config(&self) -> PathBuf {
        self.config_dir.join("mcp.toml")
    }

    /// Get the principals configuration directory
    ///
    /// Path: `{config_dir}/principals`
    #[must_use]
    pub fn principals_root_dir(&self) -> PathBuf {
        self.config_dir.join("principals")
    }

    /// Get a specific principal's configuration directory
    ///
    /// Path: `{config_dir}/principals/{principal}`
    #[must_use]
    pub fn principal_dir(&self, principal: &str) -> PathBuf {
        self.principals_root_dir().join(principal)
    }

    /// Get the path to a principal's config file
    ///
    /// Path: `{config_dir}/principals/{principal}/principal.toml`
    #[must_use]
    pub fn principal_config(&self, principal: &str) -> PathBuf {
        self.principal_dir(principal).join("principal.toml")
    }

    /// Get the path to a principal's agent prompts directory
    ///
    /// Path: `{config_dir}/principals/{principal}/agents`
    #[must_use]
    pub fn principal_agents_dir(&self, principal: &str) -> PathBuf {
        self.principal_dir(principal).join("agents")
    }

    /// Get a principal's memory directory
    ///
    /// Path: `{data_dir}/principals/{principal}/memory`
    #[must_use]
    pub fn principal_memory_dir(&self, principal: &str) -> PathBuf {
        self.data_dir.join("principals").join(principal).join("memory")
    }

    /// Get a principal's sessions directory
    ///
    /// Path: `{data_dir}/principals/{principal}/memory/sessions`
    #[must_use]
    pub fn principal_sessions_dir(&self, principal: &str) -> PathBuf {
        self.principal_memory_dir(principal).join("sessions")
    }

    /// Get the runtime directory
    ///
    /// Path: `{config_dir}/runtime`
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.config_dir.join("runtime")
    }

    /// Get the runtime identity file path
    ///
    /// Path: `{config_dir}/runtime/identity.toml`
    #[must_use]
    pub fn runtime_identity(&self) -> PathBuf {
        self.runtime_dir().join("identity.toml")
    }

    /// Get the runtime metadata file path
    ///
    /// Path: `{config_dir}/runtime/runtime.toml`
    #[must_use]
    pub fn runtime_metadata(&self) -> PathBuf {
        self.runtime_dir().join("runtime.toml")
    }

    /// Get the known runtimes file path
    ///
    /// Path: `{config_dir}/runtime/known_runtimes.toml`
    #[must_use]
    pub fn known_runtimes(&self) -> PathBuf {
        self.runtime_dir().join("known_runtimes.toml")
    }

    /// Get the auth config file path
    ///
    /// Path: `{config_dir}/runtime/auth_config.toml`
    #[must_use]
    pub fn auth_config(&self) -> PathBuf {
        self.runtime_dir().join("auth_config.toml")
    }

    /// Get the API keys file path
    ///
    /// Path: `{config_dir}/runtime/api_keys.toml`
    #[must_use]
    pub fn api_keys(&self) -> PathBuf {
        self.runtime_dir().join("api_keys.toml")
    }

    /// Get the pekohub config file path
    ///
    /// Path: `{config_dir}/runtime/pekohub.toml`
    #[must_use]
    pub fn pekohub_config(&self) -> PathBuf {
        self.runtime_dir().join("pekohub.toml")
    }

    /// Get the encrypted vault file path
    ///
    /// Path: `{config_dir}/vault.enc`
    #[must_use]
    pub fn vault(&self) -> PathBuf {
        self.config_dir.join("vault.enc")
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

    /// Get the Agents directory
    ///
    /// Path: `{data_dir}/agents`
    #[must_use]
    pub fn agents_dir(&self) -> PathBuf {
        self.data_dir.join("agents")
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

    /// Get the agent-scoped sessions root directory
    ///
    /// Path: `{data_dir}/sessions/{agent}`
    #[must_use]
    pub fn agent_sessions_root(&self, agent: &str) -> PathBuf {
        self.sessions_root().join(agent)
    }

    /// Get the personal sessions directory for an agent
    ///
    /// Path: `{data_dir}/sessions/{agent}/personal`
    #[must_use]
    pub fn agent_personal_sessions_dir(&self, agent: &str) -> PathBuf {
        self.agent_sessions_root(agent).join("personal")
    }

    /// Get the team-context sessions directory for an agent
    ///
    /// Path: `{data_dir}/sessions/{agent}/{team}`
    #[must_use]
    pub fn agent_team_sessions_dir(&self, agent: &str, team: &str) -> PathBuf {
        self.agent_sessions_root(agent).join(team)
    }

    /// Get the path to an agent's sessions directory
    ///
    /// If `team` is `None`, returns the personal sessions directory.
    /// If `team` is `Some`, returns the team-context sessions directory.
    #[must_use]
    pub fn agent_sessions_dir(&self, agent: &str, team: Option<&str>) -> PathBuf {
        match team {
            Some(team) => self.agent_team_sessions_dir(agent, team),
            None => self.agent_personal_sessions_dir(agent),
        }
    }

    /// Get the path to an agent's session file
    ///
    /// Path: `{data_dir}/sessions/{agent}/personal/{session_id}.jsonl` (no team)
    /// or `{data_dir}/sessions/{agent}/{team}/{session_id}.jsonl` (with team)
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

    /// Get the agent-scoped workspaces root directory
    ///
    /// Path: `{data_dir}/workspaces/{agent}`
    #[must_use]
    pub fn agent_workspaces_root(&self, agent: &str) -> PathBuf {
        self.workspaces_root().join(agent)
    }

    /// Get the personal workspace directory for an agent
    ///
    /// Path: `{data_dir}/workspaces/{agent}/personal`
    #[must_use]
    pub fn agent_personal_workspace(&self, agent: &str) -> PathBuf {
        self.agent_workspaces_root(agent).join("personal")
    }

    /// Get the team-context workspace directory for an agent
    ///
    /// Path: `{data_dir}/workspaces/{agent}/{team}`
    #[must_use]
    pub fn agent_team_workspace(&self, agent: &str, team: &str) -> PathBuf {
        self.agent_workspaces_root(agent).join(team)
    }

    /// Get the path to an agent's workspace directory
    ///
    /// If `team` is `None`, returns the personal workspace directory.
    /// If `team` is `Some`, returns the team-context workspace directory.
    #[must_use]
    pub fn agent_workspace(&self, agent: &str, team: Option<&str>) -> PathBuf {
        match team {
            Some(team) => self.agent_team_workspace(agent, team),
            None => self.agent_personal_workspace(agent),
        }
    }

    /// Get the path to an agent's workspace directory with explicit team
    ///
    /// Same as `agent_workspace` but requires an explicit team.
    #[must_use]
    pub fn agent_workspace_with_team(&self, agent: &str, team: &str) -> PathBuf {
        self.agent_workspace(agent, Some(team))
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
    /// Creates the agent's config directory at `{config_dir}/agents/{agent}`.
    pub fn ensure_agent_config_dirs(&self, agent: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.agent_dir(agent))?;
        Ok(())
    }

    /// Ensure an agent's data directories exist
    ///
    /// Creates the appropriate sessions and workspace directories for the agent.
    pub fn ensure_agent_data_dirs(&self, agent: &str, team: Option<&str>) -> std::io::Result<()> {
        std::fs::create_dir_all(self.agent_sessions_dir(agent, team))?;
        std::fs::create_dir_all(self.agent_workspace(agent, team))?;
        Ok(())
    }

    /// Ensure all directories for an agent exist (both config and data)
    pub fn ensure_agent_dirs(&self, agent: &str, team: Option<&str>) -> std::io::Result<()> {
        self.ensure_agent_config_dirs(agent)?;
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
        Ok(("default".to_string(), identifier.to_string()))
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
        // Some earlier tests (notably subagent_integration_tests) leak a
        // temp `PEKO_HOME` via `Box::leak`-ed fixtures, so by the time this
        // test runs the env var may already be set. Clear it for the
        // duration of the assertion so we exercise the *default* branch
        // (home_dir().join(".peko")).
        let saved_peko_home = std::env::var("PEKO_HOME").ok();
        // SAFETY: tests in the same process don't run in parallel for
        // env-var-sensitive paths (cargo test default), and we restore the
        // value immediately below.
        unsafe { std::env::remove_var("PEKO_HOME") };
        let resolver = PathResolver::new();
        let config_dir = resolver.config_dir().to_string_lossy().to_string();
        if let Some(v) = saved_peko_home {
            // SAFETY: same as above.
            unsafe { std::env::set_var("PEKO_HOME", v) };
        }
        assert!(
            config_dir.contains(".peko"),
            "default config dir should contain '.peko', got: {config_dir}"
        );
    }

    // ====================================================================================
    // New Layout Path Tests
    // ====================================================================================

    #[test]
    fn test_new_layout_agent_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(resolver.agents_root_dir(), PathBuf::from("/config/agents"));

        assert_eq!(
            resolver.agent_dir("alice"),
            PathBuf::from("/config/agents/alice")
        );

        assert_eq!(
            resolver.agent_config("alice"),
            PathBuf::from("/config/agents/alice/config.toml")
        );

        assert_eq!(
            resolver.agent_memberships("alice"),
            PathBuf::from("/config/agents/alice/memberships.toml")
        );
    }

    #[test]
    fn test_new_layout_team_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(
            resolver.team_members("engineering"),
            PathBuf::from("/config/teams/engineering/members.toml")
        );
    }

    #[test]
    fn test_new_layout_session_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(
            resolver.agent_personal_sessions_dir("alice"),
            PathBuf::from("/data/sessions/alice/personal")
        );

        assert_eq!(
            resolver.agent_team_sessions_dir("alice", "engineering"),
            PathBuf::from("/data/sessions/alice/engineering")
        );
    }

    #[test]
    fn test_new_layout_workspace_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(
            resolver.agent_personal_workspace("alice"),
            PathBuf::from("/data/workspaces/alice/personal")
        );

        assert_eq!(
            resolver.agent_team_workspace("alice", "engineering"),
            PathBuf::from("/data/workspaces/alice/engineering")
        );
    }

    #[test]
    fn test_agent_sessions_dir() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(
            resolver.agent_sessions_dir("alice", None),
            PathBuf::from("/data/sessions/alice/personal")
        );

        assert_eq!(
            resolver.agent_sessions_dir("alice", Some("engineering")),
            PathBuf::from("/data/sessions/alice/engineering")
        );
    }

    #[test]
    fn test_agent_workspace() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(
            resolver.agent_workspace("alice", None),
            PathBuf::from("/data/workspaces/alice/personal")
        );

        assert_eq!(
            resolver.agent_workspace("alice", Some("engineering")),
            PathBuf::from("/data/workspaces/alice/engineering")
        );
    }

    #[test]
    fn test_agent_exists() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir, cache_dir);

        // Create agent
        let agent_dir = config_dir.join("agents").join("alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("config.toml"), "").unwrap();

        assert!(resolver.agent_exists("alice"));
        assert!(!resolver.agent_exists("bob"));
    }

    #[test]
    fn test_ensure_agent_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);

        // Ensure dirs for personal context
        resolver.ensure_agent_dirs("alice", None).unwrap();

        assert!(config_dir.join("agents").join("alice").exists());
        assert!(data_dir
            .join("sessions")
            .join("alice")
            .join("personal")
            .exists());
        assert!(data_dir
            .join("workspaces")
            .join("alice")
            .join("personal")
            .exists());

        // Ensure dirs for team context
        resolver
            .ensure_agent_dirs("alice", Some("engineering"))
            .unwrap();

        assert!(data_dir
            .join("sessions")
            .join("alice")
            .join("engineering")
            .exists());
        assert!(data_dir
            .join("workspaces")
            .join("alice")
            .join("engineering")
            .exists());
    }
}
