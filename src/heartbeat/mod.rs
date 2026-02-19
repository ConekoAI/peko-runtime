//! Heartbeat system for periodic task execution
//!
//! Reads HEARTBEAT.md and executes tasks on a scheduled interval.

use anyhow::Result;
use std::path::PathBuf;
use tokio::time::{self, Duration};
use tracing::{info, warn};

/// Heartbeat configuration
#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    /// Whether heartbeat is enabled
    pub enabled: bool,
    /// Interval between heartbeats in minutes
    pub interval_minutes: u64,
    /// Workspace directory containing HEARTBEAT.md
    pub workspace_dir: PathBuf,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: 15,
            workspace_dir: PathBuf::from("."),
        }
    }
}

/// Heartbeat engine - reads HEARTBEAT.md and executes tasks periodically
pub struct HeartbeatEngine {
    config: HeartbeatConfig,
}

impl HeartbeatEngine {
    /// Create a new heartbeat engine
    #[must_use] 
    pub fn new(config: HeartbeatConfig) -> Self {
        Self { config }
    }

    /// Start the heartbeat loop (runs until cancelled)
    pub async fn run(&self) -> Result<()> {
        if !self.config.enabled {
            info!("Heartbeat disabled");
            return Ok(());
        }

        let interval_mins = self.config.interval_minutes.max(5);
        info!("💓 Heartbeat started: every {} minutes", interval_mins);

        let mut interval = time::interval(Duration::from_secs(interval_mins * 60));

        loop {
            interval.tick().await;

            match self.tick().await {
                Ok(tasks) => {
                    if tasks > 0 {
                        info!("💓 Heartbeat: processed {} tasks", tasks);
                    }
                }
                Err(e) => {
                    warn!("💓 Heartbeat error: {}", e);
                }
            }
        }
    }

    /// Single heartbeat tick - read HEARTBEAT.md and return task count
    pub async fn tick(&self) -> Result<usize> {
        let tasks = self.collect_tasks().await?;
        Ok(tasks.len())
    }

    /// Read HEARTBEAT.md and return all parsed tasks
    pub async fn collect_tasks(&self) -> Result<Vec<String>> {
        let heartbeat_path = self.config.workspace_dir.join("HEARTBEAT.md");

        if !heartbeat_path.exists() {
            return Ok(Vec::new());
        }

        let content = tokio::fs::read_to_string(&heartbeat_path).await?;
        Ok(Self::parse_tasks(&content))
    }

    /// Parse tasks from HEARTBEAT.md (lines starting with `- `)
    #[must_use] 
    pub fn parse_tasks(content: &str) -> Vec<String> {
        content
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                trimmed.strip_prefix("- ").map(ToString::to_string)
            })
            .collect()
    }

    /// Create a default HEARTBEAT.md if it doesn't exist
    pub async fn ensure_heartbeat_file(workspace_dir: &PathBuf) -> Result<()> {
        let path = workspace_dir.join("HEARTBEAT.md");

        if !path.exists() {
            let default = "# Periodic Tasks

# Add tasks below (one per line, starting with `- `)
# The agent will check this file on each heartbeat tick.
#
# Examples:
# - Check my email for important messages
# - Review my calendar for upcoming events
# - Check the weather forecast
";
            tokio::fs::write(&path, default).await?;
            info!("Created default HEARTBEAT.md at {:?}", path);
        }
        Ok(())
    }
}

/// Run heartbeat tasks and return results
pub async fn execute_tasks(tasks: Vec<String>) -> Vec<(String, Result<String>)> {
    let mut results = Vec::new();

    for task in tasks {
        // For now, just return the task as a result
        // In a full implementation, this would execute the task
        results.push((task.clone(), Ok(format!("Task queued: {task}"))));
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tasks_basic() {
        let content = "# Tasks\n\n- Check email\n- Review calendar\nNot a task\n- Third task";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0], "Check email");
        assert_eq!(tasks[1], "Review calendar");
        assert_eq!(tasks[2], "Third task");
    }

    #[test]
    fn parse_tasks_empty_content() {
        assert!(HeartbeatEngine::parse_tasks("").is_empty());
    }

    #[test]
    fn parse_tasks_only_comments() {
        let tasks = HeartbeatEngine::parse_tasks("# No tasks here\n\nJust comments\n# Another");
        assert!(tasks.is_empty());
    }

    #[test]
    fn parse_tasks_with_leading_whitespace() {
        let content = "  - Indented task\n\t- Tab indented";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0], "Indented task");
        assert_eq!(tasks[1], "Tab indented");
    }

    #[test]
    fn parse_tasks_dash_without_space_ignored() {
        let content = "- Real task\n-\n- Another";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0], "Real task");
        assert_eq!(tasks[1], "Another");
    }

    #[test]
    fn parse_tasks_unicode() {
        let content = "- Check email 📧\n- Review calendar 📅\n- 日本語タスク";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 3);
        assert!(tasks[0].contains("📧"));
        assert!(tasks[2].contains("日本語"));
    }

    #[tokio::test]
    async fn ensure_heartbeat_file_creates_file() {
        let dir = std::env::temp_dir().join("pekobot_test_heartbeat");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        HeartbeatEngine::ensure_heartbeat_file(&dir).await.unwrap();

        let path = dir.join("HEARTBEAT.md");
        assert!(path.exists());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("Periodic Tasks"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn tick_returns_zero_when_no_file() {
        let dir = std::env::temp_dir().join("pekobot_test_tick_no_file");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let engine = HeartbeatEngine::new(HeartbeatConfig {
            enabled: true,
            interval_minutes: 30,
            workspace_dir: dir.clone(),
        });

        let count = engine.tick().await.unwrap();
        assert_eq!(count, 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn tick_counts_tasks_from_file() {
        let dir = std::env::temp_dir().join("pekobot_test_tick_count");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        tokio::fs::write(dir.join("HEARTBEAT.md"), "- A\n- B\n- C")
            .await
            .unwrap();

        let engine = HeartbeatEngine::new(HeartbeatConfig {
            enabled: true,
            interval_minutes: 30,
            workspace_dir: dir.clone(),
        });

        let count = engine.tick().await.unwrap();
        assert_eq!(count, 3);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
