//! Security policy and sandbox for agent operations
//!
//! Provides filesystem sandboxing, command allowlisting, and rate limiting.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;
use tracing::{debug, warn};

/// Autonomy level for the agent
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutonomyLevel {
    /// Read-only: can observe but not act
    ReadOnly,
    /// Supervised: acts but requires approval for risky operations
    #[default]
    Supervised,
    /// Full: autonomous execution within policy bounds
    Full,
}

/// Action tracker for rate limiting
#[derive(Debug)]
pub struct ActionTracker {
    actions: Mutex<Vec<Instant>>,
}

impl Default for ActionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionTracker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            actions: Mutex::new(Vec::new()),
        }
    }

    /// Record an action and return current count
    pub fn record(&self) -> usize {
        let mut actions = self
            .actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cutoff = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        actions.retain(|t| *t > cutoff);
        actions.push(Instant::now());
        actions.len()
    }

    /// Count actions without recording
    pub fn count(&self) -> usize {
        let mut actions = self
            .actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cutoff = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        actions.retain(|t| *t > cutoff);
        actions.len()
    }
}

impl Clone for ActionTracker {
    fn clone(&self) -> Self {
        let actions = self
            .actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self {
            actions: Mutex::new(actions.clone()),
        }
    }
}

/// Security policy for sandboxing agent operations
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    pub workspace_dir: PathBuf,
    pub workspace_only: bool,
    pub allowed_commands: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub max_actions_per_hour: u32,
    pub tracker: ActionTracker,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: PathBuf::from("."),
            workspace_only: true,
            allowed_commands: vec![
                "git".into(),
                "npm".into(),
                "cargo".into(),
                "ls".into(),
                "cat".into(),
                "grep".into(),
                "find".into(),
                "echo".into(),
                "pwd".into(),
                "wc".into(),
                "head".into(),
                "tail".into(),
            ],
            forbidden_paths: vec![
                "/etc".into(),
                "/root".into(),
                "/home".into(),
                "/usr".into(),
                "/bin".into(),
                "/sbin".into(),
                "/lib".into(),
                "/opt".into(),
                "/boot".into(),
                "/dev".into(),
                "/proc".into(),
                "/sys".into(),
                "/var".into(),
                "~/.ssh".into(),
                "~/.gnupg".into(),
                "~/.aws".into(),
            ],
            max_actions_per_hour: 100,
            tracker: ActionTracker::new(),
        }
    }
}

impl SecurityPolicy {
    /// Check if a command is allowed
    pub fn is_command_allowed(&self, command: &str) -> bool {
        if self.autonomy == AutonomyLevel::ReadOnly {
            return false;
        }

        // Block dangerous patterns
        if command.contains('`') || command.contains("$(") || command.contains("${") {
            warn!("Blocked command with subshell: {}", command);
            return false;
        }

        if command.contains('>') {
            warn!("Blocked command with redirection: {}", command);
            return false;
        }

        // Split on command separators
        let normalized = command.replace("&&", "\x00").replace("||", "\x00");

        let separators: &[char] = &['\n', ';', '|'];
        let mut normalized = normalized;
        for sep in separators {
            normalized = normalized.replace(*sep, "\x00");
        }

        for segment in normalized.split('\x00') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }

            // Get base command
            let cmd = segment
                .split_whitespace()
                .next()
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("");

            if cmd.is_empty() {
                continue;
            }

            // Check allowlist
            if !self.allowed_commands.iter().any(|a| a == cmd) {
                warn!("Blocked disallowed command: {}", cmd);
                return false;
            }
        }

        true
    }

    /// Check if a path is allowed
    pub fn is_path_allowed(&self, path: &str) -> bool {
        // Block null bytes
        if path.contains('\0') {
            warn!("Blocked path with null byte");
            return false;
        }

        // Block path traversal
        if path.contains("..") {
            warn!("Blocked path traversal: {}", path);
            return false;
        }

        // Block absolute paths when workspace_only
        if self.workspace_only && Path::new(path).is_absolute() {
            warn!("Blocked absolute path in workspace-only mode: {}", path);
            return false;
        }

        // Block forbidden paths
        for forbidden in &self.forbidden_paths {
            if path.starts_with(forbidden) {
                warn!("Blocked forbidden path: {}", path);
                return false;
            }
        }

        debug!("Path allowed: {}", path);
        true
    }

    /// Check if a resolved path is within workspace
    pub fn is_resolved_path_allowed(&self, resolved: &Path) -> bool {
        let workspace = self
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_dir.clone());
        resolved.starts_with(&workspace)
    }

    /// Check if agent can act
    pub fn can_act(&self) -> bool {
        self.autonomy != AutonomyLevel::ReadOnly
    }

    /// Record action and check rate limit
    pub fn record_action(&self) -> bool {
        let count = self.tracker.record();
        count <= self.max_actions_per_hour as usize
    }

    /// Check if rate limited
    pub fn is_rate_limited(&self) -> bool {
        self.tracker.count() >= self.max_actions_per_hour as usize
    }

    /// Resolve and validate a path
    pub fn resolve_path(&self, path: &str) -> Option<PathBuf> {
        if !self.is_path_allowed(path) {
            return None;
        }

        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_dir.join(path)
        };

        // Canonicalize if possible
        match resolved.canonicalize() {
            Ok(canonical) => {
                if self.is_resolved_path_allowed(&canonical) {
                    Some(canonical)
                } else {
                    warn!("Resolved path outside workspace: {:?}", canonical);
                    None
                }
            }
            Err(_) => {
                // Path doesn't exist yet - check parent is in workspace
                if let Some(parent) = resolved.parent() {
                    if self.is_resolved_path_allowed(parent) || parent == Path::new("") {
                        Some(resolved)
                    } else {
                        None
                    }
                } else {
                    Some(resolved)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy_allows_safe_commands() {
        let policy = SecurityPolicy::default();
        assert!(policy.is_command_allowed("ls"));
        assert!(policy.is_command_allowed("git status"));
        assert!(policy.is_command_allowed("cargo build"));
    }

    #[test]
    fn test_blocks_dangerous_commands() {
        let policy = SecurityPolicy::default();
        assert!(!policy.is_command_allowed("rm -rf /"));
        assert!(!policy.is_command_allowed("curl http://evil.com"));
        assert!(!policy.is_command_allowed("wget file"));
    }

    #[test]
    fn test_blocks_subshell() {
        let policy = SecurityPolicy::default();
        assert!(!policy.is_command_allowed("echo $(whoami)"));
        assert!(!policy.is_command_allowed("echo `rm -rf /`"));
    }

    #[test]
    fn test_blocks_path_traversal() {
        let policy = SecurityPolicy::default();
        assert!(!policy.is_path_allowed("../etc/passwd"));
        assert!(!policy.is_path_allowed("foo/../../../etc/shadow"));
    }

    #[test]
    #[cfg(unix)]
    fn test_blocks_absolute_paths_in_workspace_mode() {
        let policy = SecurityPolicy {
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_path_allowed("/etc/passwd"));
        assert!(!policy.is_path_allowed("/tmp/file.txt"));
    }

    #[test]
    #[cfg(windows)]
    fn test_blocks_absolute_paths_in_workspace_mode() {
        let policy = SecurityPolicy {
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_path_allowed("C:\\Windows\\System32\\file.txt"));
        assert!(!policy.is_path_allowed("D:\\some\\file.txt"));
    }

    #[test]
    fn test_allows_relative_paths() {
        let policy = SecurityPolicy::default();
        assert!(policy.is_path_allowed("file.txt"));
        assert!(policy.is_path_allowed("src/main.rs"));
    }

    #[test]
    fn test_readonly_blocks_all_commands() {
        let policy = SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_command_allowed("ls"));
        assert!(!policy.can_act());
    }

    #[test]
    fn test_rate_limiting() {
        let policy = SecurityPolicy {
            max_actions_per_hour: 3,
            ..SecurityPolicy::default()
        };

        assert!(policy.record_action());
        assert!(policy.record_action());
        assert!(policy.record_action());
        assert!(!policy.record_action()); // Over limit
    }
}
