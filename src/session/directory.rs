//! Session Directory Management
//!
//! Responsible ONLY for directory operations - no session logic.
//! This module provides explicit directory management to prevent
//! side effects during session lookup operations.

use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::debug;

use crate::session::safe_filename_component;

/// Session directory manager
///
/// Manages session directory paths without implicit filesystem operations.
/// Directory creation is explicit via `ensure_exists()`.
pub struct SessionDirectory {
    path: PathBuf,
}

impl SessionDirectory {
    /// Create directory manager (does NOT create filesystem directory)
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Create from agent name using `PathResolver`
    #[must_use]
    pub fn for_agent(agent_name: &str) -> Self {
        let resolver = crate::common::paths::PathResolver::new();
        let path = resolver.agent_sessions_dir(agent_name);
        Self::new(path)
    }

    /// Ensure directory exists - called explicitly when needed
    pub async fn ensure_exists(&self) -> anyhow::Result<()> {
        if !self.path.exists() {
            debug!("Creating session directory: {}", self.path.display());
            fs::create_dir_all(&self.path).await?;
        }
        Ok(())
    }

    /// Get path without creating directory
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get path as `PathBuf` (cloned)
    #[must_use]
    pub fn path_buf(&self) -> PathBuf {
        self.path.clone()
    }

    /// Check if directory exists
    #[must_use]
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Get session file path
    #[must_use]
    pub fn session_file(&self, session_id: &str) -> PathBuf {
        self.path
            .join(format!("{}.jsonl", safe_filename_component(session_id)))
    }

    /// Get index file path
    #[must_use]
    pub fn index_file(&self) -> PathBuf {
        self.path.join("sessions.json")
    }

    /// Get peers file path
    #[must_use]
    pub fn peers_file(&self) -> PathBuf {
        self.path.join("peers.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_session_directory_lifecycle() {
        let temp = TempDir::new().unwrap();
        let dir_path = temp.path().join("test_sessions");

        // Create manager (does not create directory)
        let dir = SessionDirectory::new(dir_path.clone());
        assert!(!dir.exists());
        assert_eq!(dir.path(), dir_path);

        // Explicitly create directory
        dir.ensure_exists().await.unwrap();
        assert!(dir.exists());

        // Idempotent - second call is fine
        dir.ensure_exists().await.unwrap();
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn test_session_file_paths() {
        let temp = TempDir::new().unwrap();
        let dir = SessionDirectory::new(temp.path().to_path_buf());

        assert_eq!(
            dir.session_file("sess_123"),
            temp.path().join("sess_123.jsonl")
        );
        assert_eq!(dir.index_file(), temp.path().join("sessions.json"));
        assert_eq!(dir.peers_file(), temp.path().join("peers.json"));
    }

    #[test]
    fn test_for_agent() {
        let dir = SessionDirectory::for_agent("myagent");
        let path = dir.path();
        let path_str = path.to_string_lossy();

        assert!(path_str.contains("personal"));
        assert!(path_str.contains("myagent"));
    }
}
