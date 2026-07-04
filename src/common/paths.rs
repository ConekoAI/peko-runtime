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
//! │       ├── tools/                # Agent-specific tools
//! │       └── skills/               # Agent-specific skills
//! └── principals/                   # Principal container configs
//!     └── {principal}/
//!         └── principal.toml        # Principal metadata
//!
//! ## Data Directory (`{data_dir}`, e.g., `~/.local/share/peko`)
//! ```text
//! {data_dir}/
//! ├── tools/                        # Downloaded/installed tools
//! ├── cron.json                     # Cron job database
//! ├── sessions/                     # Session history (*.jsonl)
//! │   └── {agent}/                  # Agent-scoped sessions
//! │       └── personal/             # Personal sessions
//! └── workspaces/                   # Agent workspace files
//!     └── {agent}/
//!         └── personal/             # Personal workspace
//! ```

use std::path::{Path, PathBuf};

use crate::session::safe_filename_component;

/// Expand a leading `~` in a path string to the user's home directory.
///
/// - `"~/foo"` → `{home}/foo`
/// - `"~"` → `{home}`
/// - `"/abs/path"` and `"relative"` are returned unchanged
///
/// This is intentionally a string-in/string-out helper so it can be
/// applied before validating whether the result is absolute.
#[must_use]
pub fn expand_tilde(path: impl AsRef<str>) -> PathBuf {
    let path = path.as_ref();
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let mut home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        home.push(rest);
        return home;
    }
    PathBuf::from(path)
}

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
/// This struct provides methods to resolve paths for agents,
/// sessions, and workspaces. It uses the configured base directories
/// and applies the personal-only path structure consistently.
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
        self.data_dir
            .join("principals")
            .join(principal)
            .join("memory")
    }

    /// Get a principal's sessions directory
    ///
    /// Path: `{data_dir}/principals/{principal}/memory/sessions`
    #[must_use]
    pub fn principal_sessions_dir(&self, principal: &str) -> PathBuf {
        self.principal_memory_dir(principal).join("sessions")
    }

    /// Get a principal's identity storage directory.
    ///
    /// Path: `{data_dir}/principals/{principal}/identity`
    #[must_use]
    pub fn principal_identity_dir(&self, principal: &str) -> PathBuf {
        self.data_dir
            .join("principals")
            .join(principal)
            .join("identity")
    }

    /// Get the path to a principal's identity storage directory.
    ///
    /// Alias for [`Self::principal_identity_dir`].
    #[must_use]
    pub fn principal_identity_path(&self, principal: &str) -> PathBuf {
        self.principal_identity_dir(principal)
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
    pub fn agent_sessions_dir(&self, agent: &str) -> PathBuf {
        self.agent_sessions_root(agent).join("personal")
    }

    /// Get the path to an agent's session file
    ///
    /// Path: `{data_dir}/sessions/{agent}/personal/{session_id}.jsonl`
    #[must_use]
    pub fn agent_session_file(&self, agent: &str, session_id: &str) -> PathBuf {
        self.agent_sessions_dir(agent)
            .join(format!("{}.jsonl", safe_filename_component(session_id)))
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
    pub fn agent_workspace(&self, agent: &str) -> PathBuf {
        self.agent_workspaces_root(agent).join("personal")
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
    /// Creates the personal sessions and workspace directories for the agent.
    pub fn ensure_agent_data_dirs(&self, agent: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.agent_sessions_dir(agent))?;
        std::fs::create_dir_all(self.agent_workspace(agent))?;
        Ok(())
    }

    /// Ensure all directories for an agent exist (both config and data)
    pub fn ensure_agent_dirs(&self, agent: &str) -> std::io::Result<()> {
        self.ensure_agent_config_dirs(agent)?;
        self.ensure_agent_data_dirs(agent)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde_home() {
        let home = dirs::home_dir().expect("should have home dir");
        assert_eq!(expand_tilde("~/foo"), home.join("foo"));
        assert_eq!(expand_tilde("~"), home);
    }

    #[test]
    fn test_expand_tilde_unchanged() {
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("relative"), PathBuf::from("relative"));
        assert_eq!(expand_tilde("./relative"), PathBuf::from("./relative"));
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
    fn test_new_layout_session_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(
            resolver.agent_sessions_dir("alice"),
            PathBuf::from("/data/sessions/alice/personal")
        );

        assert_eq!(
            resolver.agent_session_file("alice", "sess-1"),
            PathBuf::from("/data/sessions/alice/personal/sess-1.jsonl")
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
            resolver.agent_workspace("alice"),
            PathBuf::from("/data/workspaces/alice/personal")
        );
    }

    #[test]
    fn test_ensure_agent_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);

        resolver.ensure_agent_dirs("alice").unwrap();

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
}
