//! Configuration types for process supervision

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Default timeout for graceful process shutdown
pub const DEFAULT_KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Default base backoff for restart (milliseconds)
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 1_000;

/// Default max backoff for restart (milliseconds)
pub const DEFAULT_BACKOFF_MAX_MS: u64 = 60_000;

/// Configuration for spawning a child process
#[derive(Debug, Clone)]
pub struct ProcessSpawnConfig {
    /// Command to execute
    pub command: String,
    /// Arguments to pass
    pub args: Vec<String>,
    /// Environment variables
    pub env: HashMap<String, String>,
    /// Working directory
    pub cwd: Option<PathBuf>,
    /// Enable stderr logging
    pub log_stderr: bool,
    /// Kill timeout for graceful shutdown
    pub kill_timeout: Duration,
    /// Auto-detect interpreter for script files (.py, .js)
    pub auto_interpreter: bool,
    /// On Windows, assign the process to a job object with Kill-On-Close.
    pub use_job_object: bool,
}

impl Default for ProcessSpawnConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            log_stderr: true,
            kill_timeout: DEFAULT_KILL_TIMEOUT,
            auto_interpreter: true,
            use_job_object: true,
        }
    }
}

impl ProcessSpawnConfig {
    /// Create a new config with defaults
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            ..Default::default()
        }
    }

    /// Set arguments
    #[must_use]
    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Set an environment variable
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set working directory
    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    /// Enable/disable stderr logging
    #[must_use]
    pub fn log_stderr(mut self, enabled: bool) -> Self {
        self.log_stderr = enabled;
        self
    }

    /// Set kill timeout
    #[must_use]
    pub fn kill_timeout(mut self, secs: u64) -> Self {
        self.kill_timeout = Duration::from_secs(secs);
        self
    }

    /// Enable/disable auto interpreter detection
    #[must_use]
    pub fn auto_interpreter(mut self, enabled: bool) -> Self {
        self.auto_interpreter = enabled;
        self
    }

    /// Enable/disable Windows job object assignment
    #[must_use]
    pub fn use_job_object(mut self, enabled: bool) -> Self {
        self.use_job_object = enabled;
        self
    }
}

/// Policy for restarting a crashed runtime
#[derive(Debug, Clone)]
pub struct RestartPolicy {
    /// Maximum number of restart attempts (0 = no restart)
    pub max_restarts: u32,
    /// Base backoff duration in milliseconds
    pub backoff_base_ms: u64,
    /// Maximum backoff duration in milliseconds
    pub backoff_max_ms: u64,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            backoff_base_ms: DEFAULT_BACKOFF_BASE_MS,
            backoff_max_ms: DEFAULT_BACKOFF_MAX_MS,
        }
    }
}

impl RestartPolicy {
    /// Create a default restart policy
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a policy that never restarts
    #[must_use]
    pub fn never() -> Self {
        Self {
            max_restarts: 0,
            ..Default::default()
        }
    }

    /// Create a policy with unlimited restarts
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            max_restarts: u32::MAX,
            ..Default::default()
        }
    }

    /// Calculate backoff duration for a given restart count
    #[must_use]
    pub fn backoff_for(&self, restart_count: u32) -> Duration {
        let exponent = restart_count.min(10) as u64; // Cap to avoid overflow
        let ms = self
            .backoff_base_ms
            .saturating_mul(2_u64.saturating_pow(exponent as u32));
        Duration::from_millis(ms.min(self.backoff_max_ms))
    }
}

/// Unified spawn configuration for any runtime kind
#[derive(Debug, Clone)]
pub enum RuntimeSpawnConfig {
    /// Spawn a child process
    Process(ProcessSpawnConfig),
    /// Spawn an in-process async task (no process config needed)
    Task {
        /// Task name for logging
        name: String,
    },
    /// Connect to an external endpoint
    External {
        /// Endpoint URL
        endpoint: String,
        /// Connection timeout
        connect_timeout: Duration,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_spawn_config_builder() {
        let config = ProcessSpawnConfig::new("node")
            .args(vec!["bot.js".to_string()])
            .env("TOKEN", "secret")
            .cwd("/tmp")
            .log_stderr(false)
            .kill_timeout(10)
            .use_job_object(false);

        assert_eq!(config.command, "node");
        assert_eq!(config.args, vec!["bot.js"]);
        assert_eq!(config.env.get("TOKEN"), Some(&"secret".to_string()));
        assert_eq!(config.cwd, Some(PathBuf::from("/tmp")));
        assert!(!config.log_stderr);
        assert_eq!(config.kill_timeout, Duration::from_secs(10));
        assert!(!config.use_job_object);
    }

    #[test]
    fn test_restart_policy_backoff() {
        let policy = RestartPolicy::default();

        // First restart: base * 2^0 = 1000ms
        assert_eq!(policy.backoff_for(0), Duration::from_millis(1_000));
        // Second restart: base * 2^1 = 2000ms
        assert_eq!(policy.backoff_for(1), Duration::from_millis(2_000));
        // Third restart: base * 2^2 = 4000ms
        assert_eq!(policy.backoff_for(2), Duration::from_millis(4_000));
    }

    #[test]
    fn test_restart_policy_never() {
        let policy = RestartPolicy::never();
        assert_eq!(policy.max_restarts, 0);
    }

    #[test]
    fn test_restart_policy_max_backoff() {
        let policy = RestartPolicy {
            backoff_base_ms: 1_000,
            backoff_max_ms: 10_000,
            ..Default::default()
        };

        // Should cap at max
        assert_eq!(policy.backoff_for(100), Duration::from_millis(10_000));
    }
}
