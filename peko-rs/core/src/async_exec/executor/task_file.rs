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

    // No `read`: task files are a write-only audit trail — nothing reads
    // them back (the registry is the in-memory source of truth). A reader
    // was carried for a durability story that never landed; dropped in the
    // 2026-09-27 consolidation (ADR-063 (P2-2)).

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_land_at_a_filesystem_safe_path_with_the_receipt_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let writer = TaskFileWriter::new(dir.path().join("tasks"));
        let mut record = TaskFileRecord::new("Bash:a/b".into(), "Bash".into());
        record.set_running();
        record.set_timed_out("too slow".into());
        writer.write(&record).await.unwrap();

        let path = writer.task_file_path("Bash:a/b");
        assert_eq!(path, dir.path().join("tasks").join("Bash_a_b.json"));
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(on_disk["status"], "timed_out");
        assert_eq!(on_disk["_async_status"], "timed_out");
        assert_eq!(on_disk["error"], "too slow");
        assert!(on_disk["started_at"].is_string() && on_disk["completed_at"].is_string());
    }

    #[tokio::test]
    async fn cleanup_removes_only_files_older_than_the_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let writer = TaskFileWriter::new(dir.path().to_path_buf());
        for id in ["old", "fresh"] {
            writer
                .write(&TaskFileRecord::new(id.into(), "Bash".into()))
                .await
                .unwrap();
        }
        let day_ago = std::time::SystemTime::now() - Duration::from_hours(25);
        std::fs::File::options()
            .write(true)
            .open(writer.task_file_path("old"))
            .unwrap()
            .set_modified(day_ago)
            .unwrap();

        assert_eq!(
            writer.cleanup_old(Duration::from_hours(24)).await.unwrap(),
            1
        );
        assert!(!writer.task_file_path("old").exists());
        assert!(writer.task_file_path("fresh").exists());

        let missing = TaskFileWriter::new(dir.path().join("never-created"));
        assert_eq!(missing.cleanup_old(Duration::ZERO).await.unwrap(), 0);
    }
}
