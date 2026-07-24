//! On-disk task file records for agent polling

use super::types::AsyncTaskId;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// On-disk record for polling async task status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFileRecord {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    /// Mirror of `status` exposed as `_async_status` for LLM receipt matching
    #[serde(rename = "_async_status")]
    pub async_status: String,
    pub status: String,
    /// Parameters the agent used to invoke the tool (audit transparency)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Opaque result — tool-specific structure lives inside
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_requested: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_mode: Option<String>,
}

impl TaskFileRecord {
    pub fn new(task_id: AsyncTaskId, tool_name: String) -> Self {
        Self {
            task_id,
            tool_name,
            async_status: "pending".to_string(),
            status: "pending".to_string(),
            params: None,
            result: None,
            error: None,
            started_at: None,
            completed_at: None,
            timeout_requested: None,
            callback_mode: None,
        }
    }

    fn sync_async_status(&mut self) {
        self.async_status = self.status.clone();
    }

    pub fn set_running(&mut self) {
        self.status = "running".to_string();
        self.sync_async_status();
        self.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    pub fn set_completed(&mut self, result: serde_json::Value) {
        self.status = "completed".to_string();
        self.sync_async_status();
        self.result = Some(result);
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }

    pub fn set_failed(&mut self, error: String) {
        self.status = "failed".to_string();
        self.sync_async_status();
        self.error = Some(error);
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }

    pub fn set_timed_out(&mut self, error: String) {
        self.status = "timed_out".to_string();
        self.sync_async_status();
        self.error = Some(error);
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }
}

/// Writes task file records to disk for agent polling
#[derive(Debug, Clone)]
pub struct TaskFileWriter {
    base_dir: PathBuf,
}

impl TaskFileWriter {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn task_file_path(&self, task_id: &str) -> PathBuf {
        let safe_id = task_id.replace(':', "_").replace('/', "_");
        self.base_dir.join(format!("{safe_id}.json"))
    }

    pub async fn write(&self, record: &TaskFileRecord) -> Result<()> {
        let path = self.task_file_path(&record.task_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(record)?;
        tokio::fs::write(&path, json).await?;
        Ok(())
    }

    pub async fn read(&self, task_id: &str) -> Result<TaskFileRecord> {
        let path = self.task_file_path(task_id);
        let content = tokio::fs::read_to_string(&path).await?;
        let record = serde_json::from_str(&content)?;
        Ok(record)
    }

    pub async fn cleanup_old(&self, max_age: Duration) -> Result<usize> {
        if !self.base_dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        let mut entries = tokio::fs::read_dir(&self.base_dir).await?;
        let cutoff = std::time::SystemTime::now() - max_age;
        while let Some(entry) = entries.next_entry().await? {
            let metadata = entry.metadata().await?;
            if let Ok(modified) = metadata.modified() {
                if modified < cutoff {
                    tokio::fs::remove_file(entry.path()).await.ok();
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}
