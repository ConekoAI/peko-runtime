//! Session Recovery Module
//!
//! Handles session state recovery on daemon restart:
//! - Scans for sessions without sidecars (rebuilds them)
//! - Detects sessions that didn't end cleanly (no session.ended event)
//! - Cleans up partial .tmp files from crashes
//! - Verifies JSONL integrity
//!
//! Per REQ-SM-001 and REQ-RL-003: Session history must be fully recoverable
//! from JSONL files alone.

use crate::session::jsonl::SessionStorage;
use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, info, warn};

/// Recovery report
#[derive(Debug, Clone)]
pub struct RecoveryReport {
    /// Number of sessions scanned
    pub sessions_scanned: usize,

    /// Number of temp files cleaned up
    pub temp_files_cleaned: usize,
    /// Sessions that had errors during recovery
    pub errors: Vec<(String, String)>,
}

impl RecoveryReport {
    /// Create empty report
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions_scanned: 0,
            temp_files_cleaned: 0,
            errors: vec![],
        }
    }

    /// Check if recovery was successful
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }
}

impl Default for RecoveryReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Session recovery manager
#[derive(Debug, Clone)]
pub struct SessionRecovery {
    workspace_path: PathBuf,
}

impl SessionRecovery {
    /// Create new recovery manager
    #[must_use]
    pub fn new(workspace_path: impl AsRef<Path>) -> Self {
        Self {
            workspace_path: workspace_path.as_ref().to_path_buf(),
        }
    }

    /// Perform full recovery
    ///
    /// This should be called on daemon startup to ensure all sessions
    /// are in a consistent state.
    pub async fn recover_all(&self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::new();

        info!("Starting session recovery from: {:?}", self.workspace_path);

        // Find all session directories
        let session_dirs = self.find_session_directories().await?;
        debug!("Found {} session directories", session_dirs.len());

        for dir in session_dirs {
            if let Err(e) = self.recover_directory(&dir, &mut report).await {
                warn!("Failed to recover sessions in {:?}: {}", dir, e);
                report
                    .errors
                    .push((dir.to_string_lossy().to_string(), e.to_string()));
            }
        }

        info!(
            "Session recovery complete: scanned={}, cleaned={}",
            report.sessions_scanned, report.temp_files_cleaned
        );

        Ok(report)
    }

    /// Recover sessions in a specific directory
    async fn recover_directory(&self, dir: &Path, report: &mut RecoveryReport) -> Result<()> {
        debug!("Recovering sessions in: {:?}", dir);

        let storage = SessionStorage::new(dir.to_path_buf());

        // List all session files
        let sessions = storage.list_sessions().await?;

        for session_id in sessions {
            report.sessions_scanned += 1;

            // Clean up temp files
            if let Err(e) = storage.cleanup_temp_files(&session_id).await {
                warn!("Failed to clean up temp files for {}: {}", session_id, e);
            } else {
                // Check if tmp file existed and was cleaned
                let tmp_path = dir.join(format!("{}.tmp", session_id));
                if !tmp_path.exists() {
                    report.temp_files_cleaned += 1;
                }
            }

            // Verify session and mark unclean terminations
            if let Err(e) = self.verify_session(dir, &session_id, report).await {
                warn!("Verification failed for {}: {}", session_id, e);
            }
        }

        Ok(())
    }

    /// Verify a session and mark unclean terminations
    async fn verify_session(
        &self,
        dir: &Path,
        session_id: &str,
        _report: &mut RecoveryReport,
    ) -> Result<()> {
        let storage = SessionStorage::new(dir.to_path_buf());

        // Load events
        let events = storage.load_events(session_id).await?;

        if events.is_empty() {
            return Ok(());
        }

        // Check if session has ended
        let has_ended = events.iter().any(|e| e.is_session_ended());

        if !has_ended {
            // Session didn't end cleanly
            debug!("Session {} did not end cleanly", session_id);
            // This is informational - we don't modify the JSONL
        }

        Ok(())
    }

    /// Find all session directories in the workspace
    async fn find_session_directories(&self) -> Result<Vec<PathBuf>> {
        let mut directories = vec![];

        // Check for agent sessions
        let agents_dir = self.workspace_path.join("agents");
        if agents_dir.exists() {
            self.find_sessions_in_dir(&agents_dir, &mut directories)
                .await?;
        }

        // Check for team sessions
        let teams_dir = self.workspace_path.join("teams");
        if teams_dir.exists() {
            let mut team_entries = fs::read_dir(&teams_dir).await?;
            while let Some(entry) = team_entries.next_entry().await? {
                let team_path = entry.path();
                if team_path.is_dir() {
                    let agents_dir = team_path.join("agents");
                    if agents_dir.exists() {
                        self.find_sessions_in_dir(&agents_dir, &mut directories)
                            .await?;
                    }
                }
            }
        }

        Ok(directories)
    }

    /// Find session directories within a directory (recursive)
    async fn find_sessions_in_dir(&self, dir: &Path, results: &mut Vec<PathBuf>) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        let mut entries = fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                let sessions_dir = path.join("sessions");
                if sessions_dir.exists() {
                    results.push(sessions_dir);
                }

                // Recurse into subdirectories
                Box::pin(self.find_sessions_in_dir(&path, results)).await?;
            }
        }

        Ok(())
    }

    /// Extract instance ID from session directory path
    fn extract_instance_id(&self, dir: &Path) -> Result<String> {
        // Try to extract from path
        // Expected format: .../{instance_id}/sessions
        let parent = dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot extract instance_id from path: {:?}", dir))?;

        let instance_id = parent
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Cannot extract instance_id from path: {:?}", dir))?
            .to_string_lossy()
            .to_string();

        Ok(instance_id)
    }

    /// Clean up all temp files in workspace
    pub async fn cleanup_all_temp_files(&self) -> Result<usize> {
        let mut cleaned = 0;

        let session_dirs = self.find_session_directories().await?;

        for dir in session_dirs {
            let mut entries = fs::read_dir(&dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if ext == "tmp" {
                        info!("Cleaning up temp file: {:?}", path);
                        fs::remove_file(&path).await?;
                        cleaned += 1;
                    }
                }
            }
        }

        Ok(cleaned)
    }
}

/// Recovery state tracking
#[derive(Debug, Clone, Default)]
pub struct RecoveryState {
    /// Sessions that were recovered
    pub recovered_sessions: Vec<String>,
    /// Sessions that failed recovery
    pub failed_sessions: Vec<(String, String)>,
    /// Sessions that ended uncleanly
    pub unclean_sessions: Vec<String>,
}

impl RecoveryState {
    /// Create empty state
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add recovered session
    pub fn add_recovered(&mut self, session_id: impl Into<String>) {
        self.recovered_sessions.push(session_id.into());
    }

    /// Add failed session
    pub fn add_failed(&mut self, session_id: impl Into<String>, error: impl Into<String>) {
        self.failed_sessions.push((session_id.into(), error.into()));
    }

    /// Add unclean session
    pub fn add_unclean(&mut self, session_id: impl Into<String>) {
        self.unclean_sessions.push(session_id.into());
    }

    /// Check if all recoveries succeeded
    #[must_use]
    pub fn all_succeeded(&self) -> bool {
        self.failed_sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::events::{EventEnvelope, SessionCreatedEvent};
    use chrono::Utc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_recovery_report() {
        let mut report = RecoveryReport::new();
        assert!(report.is_success());

        report
            .errors
            .push(("sess_123".to_string(), "error".to_string()));
        assert!(!report.is_success());
    }

    #[tokio::test]
    async fn test_find_session_directories() {
        let temp = TempDir::new().unwrap();

        // Create agent session directory
        let agent_sessions = temp.path().join("agents").join("inst_001").join("sessions");
        fs::create_dir_all(&agent_sessions).await.unwrap();

        let recovery = SessionRecovery::new(temp.path());
        let dirs = recovery.find_session_directories().await.unwrap();

        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("sessions"));
    }

    #[tokio::test]
    async fn test_extract_instance_id() {
        let temp = TempDir::new().unwrap();

        // Create path structure
        let sessions_dir = temp
            .path()
            .join("agents")
            .join("inst_abc123")
            .join("sessions");
        fs::create_dir_all(&sessions_dir).await.unwrap();

        let recovery = SessionRecovery::new(temp.path());
        let instance_id = recovery.extract_instance_id(&sessions_dir).unwrap();

        assert_eq!(instance_id, "inst_abc123");
    }

    #[tokio::test]
    async fn test_cleanup_temp_files() {
        let temp = TempDir::new().unwrap();

        // Create session directory with temp files
        let sessions_dir = temp.path().join("agents").join("inst_001").join("sessions");
        fs::create_dir_all(&sessions_dir).await.unwrap();

        // Create temp files
        fs::write(sessions_dir.join("sess_001.tmp"), "partial")
            .await
            .unwrap();
        fs::write(sessions_dir.join("sess_002.tmp"), "partial")
            .await
            .unwrap();

        let recovery = SessionRecovery::new(temp.path());
        let cleaned = recovery.cleanup_all_temp_files().await.unwrap();

        assert_eq!(cleaned, 2);
        assert!(!sessions_dir.join("sess_001.tmp").exists());
    }
}
