//! Session Index Sidecar (.index.json)
//!
//! Implements per-session index files per DATA_MODEL.md §5.4:
//! - Maintained alongside each .jsonl file
//! - Provides O(1) lookup of session metadata
//! - Updated atomically on every event write
//! - Auto-generates title from first assistant response

use crate::session::events::{SessionEvent, SessionTrigger};
use crate::session::lock::FileLock;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// Session index sidecar structure (DATA_MODEL §5.4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSidecarIndex {
    /// Session ID
    pub session_id: String,
    /// Instance ID that owns this session
    pub instance_id: String,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
    /// Number of turns (user-assistant exchanges)
    pub turn_count: u32,
    /// Total number of events in the session
    pub event_count: u64,
    /// Total tokens used
    pub total_tokens: u32,
    /// Parent session ID (for branched sessions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// What triggered this session creation
    pub trigger: SessionTrigger,
    /// Whether the session has ended
    pub ended: bool,
    /// Session title (auto-generated or user-set)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl SessionSidecarIndex {
    /// Create a new sidecar index
    pub fn new(
        session_id: impl Into<String>,
        instance_id: impl Into<String>,
        trigger: SessionTrigger,
    ) -> Self {
        let now = Utc::now();
        Self {
            session_id: session_id.into(),
            instance_id: instance_id.into(),
            created_at: now,
            updated_at: now,
            turn_count: 0,
            event_count: 0,
            total_tokens: 0,
            parent_session_id: None,
            trigger,
            ended: false,
            title: None,
        }
    }

    /// Create a new sidecar for a branched session
    pub fn new_branched(
        session_id: impl Into<String>,
        instance_id: impl Into<String>,
        parent_session_id: impl Into<String>,
        trigger: SessionTrigger,
    ) -> Self {
        let mut index = Self::new(session_id, instance_id, trigger);
        index.parent_session_id = Some(parent_session_id.into());
        index
    }

    /// Update timestamp
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Mark session as ended
    pub fn mark_ended(&mut self) {
        self.ended = true;
        self.touch();
    }

    /// Increment turn count
    pub fn increment_turn(&mut self) {
        self.turn_count += 1;
        self.touch();
    }

    /// Increment event count
    pub fn increment_event(&mut self) {
        self.event_count += 1;
        self.touch();
    }

    /// Add tokens
    pub fn add_tokens(&mut self, tokens: u32) {
        self.total_tokens += tokens;
        self.touch();
    }

    /// Set title (or auto-generate from content)
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = Some(title.into());
        self.touch();
    }

    /// Auto-generate title from assistant response content
    /// Takes first 60 characters, strips newlines
    pub fn auto_generate_title(&mut self, content: &str) {
        if self.title.is_some() {
            // Title already set, don't override
            return;
        }

        let title = content
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect::<String>();

        if !title.is_empty() {
            self.set_title(title);
        }
    }
}

/// Sidecar index manager
#[derive(Debug, Clone)]
pub struct SidecarManager {
    storage_dir: PathBuf,
}

impl SidecarManager {
    /// Create new sidecar manager
    #[must_use]
    pub fn new(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    /// Get sidecar file path for a session
    pub fn sidecar_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{session_id}.index.json"))
    }

    /// Check if sidecar exists
    pub async fn exists(&self, session_id: &str) -> bool {
        self.sidecar_path(session_id).exists()
    }

    /// Create a new sidecar index atomically
    pub async fn create(
        &self,
        session_id: &str,
        instance_id: &str,
        trigger: SessionTrigger,
    ) -> Result<SessionSidecarIndex> {
        fs::create_dir_all(&self.storage_dir).await?;

        let index = SessionSidecarIndex::new(session_id, instance_id, trigger);
        self.save(session_id, &index).await?;

        debug!("Created sidecar index for session: {}", session_id);
        Ok(index)
    }

    /// Create a sidecar for a branched session
    pub async fn create_branched(
        &self,
        session_id: &str,
        instance_id: &str,
        parent_session_id: &str,
        trigger: SessionTrigger,
    ) -> Result<SessionSidecarIndex> {
        fs::create_dir_all(&self.storage_dir).await?;

        let index =
            SessionSidecarIndex::new_branched(session_id, instance_id, parent_session_id, trigger);
        self.save(session_id, &index).await?;

        debug!(
            "Created branched sidecar index for session: {} (parent: {})",
            session_id, parent_session_id
        );
        Ok(index)
    }

    /// Load sidecar index
    pub async fn load(&self, session_id: &str) -> Result<Option<SessionSidecarIndex>> {
        let path = self.sidecar_path(session_id);

        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path).await?;
        let index: SessionSidecarIndex = serde_json::from_str(&content)?;

        Ok(Some(index))
    }

    /// Save sidecar index atomically
    pub async fn save(&self, session_id: &str, index: &SessionSidecarIndex) -> Result<()> {
        let path = self.sidecar_path(session_id);

        // Acquire lock
        let _lock = FileLock::acquire(&path, 5000).await?;

        // Serialize
        let json = serde_json::to_string_pretty(index)?;

        // Write atomically
        let temp_path = path.with_extension("tmp");
        {
            let mut file = fs::File::create(&temp_path).await?;
            file.write_all(json.as_bytes()).await?;
            file.flush().await?;
        }

        fs::rename(&temp_path, &path).await?;

        Ok(())
    }

    /// Update sidecar index with an event
    pub async fn update_with_event(&self, session_id: &str, event: &SessionEvent) -> Result<()> {
        let mut index = match self.load(session_id).await? {
            Some(idx) => idx,
            None => {
                warn!(
                    "No sidecar index found for session {}, cannot update",
                    session_id
                );
                return Ok(());
            }
        };

        // Update based on event type
        match event {
            SessionEvent::UserMessage(_) => {
                index.increment_event();
            }
            SessionEvent::AssistantMessage(e) => {
                index.increment_event();
                index.increment_turn();
                index.add_tokens(e.usage.total_tokens);

                // Auto-generate title from first assistant response
                if index.turn_count == 1 {
                    index.auto_generate_title(&e.content);
                }
            }
            SessionEvent::ToolCall(_) | SessionEvent::ToolResult(_) => {
                index.increment_event();
            }
            SessionEvent::Thinking(_) => {
                // Thinking doesn't count as a turn
                index.increment_event();
            }
            SessionEvent::SpawnRequest(_) | SessionEvent::SpawnResult(_) => {
                index.increment_event();
            }
            SessionEvent::A2aSent(_) | SessionEvent::A2aReceived(_) => {
                index.increment_event();
            }
            SessionEvent::HookTrigger(_) | SessionEvent::System(_) => {
                index.increment_event();
            }
            SessionEvent::SessionEnded(_) => {
                index.increment_event();
                index.mark_ended();
            }
            SessionEvent::SessionCreated(_) => {
                // This is the first event, already counted in creation
            }
        }

        self.save(session_id, &index).await?;
        Ok(())
    }

    /// Set session title
    pub async fn set_title(&self, session_id: &str, title: impl Into<String>) -> Result<()> {
        let mut index = match self.load(session_id).await? {
            Some(idx) => idx,
            None => {
                return Err(anyhow::anyhow!(
                    "No sidecar index found for session {}",
                    session_id
                ));
            }
        };

        index.set_title(title);
        self.save(session_id, &index).await?;

        info!("Set title for session {}: {:?}", session_id, index.title);
        Ok(())
    }

    /// Mark session as ended
    pub async fn mark_ended(
        &self,
        session_id: &str,
        reason: crate::session::events::SessionEndReason,
    ) -> Result<()> {
        let mut index = match self.load(session_id).await? {
            Some(idx) => idx,
            None => {
                return Err(anyhow::anyhow!(
                    "No sidecar index found for session {}",
                    session_id
                ));
            }
        };

        index.mark_ended();
        self.save(session_id, &index).await?;

        info!("Marked session {} as ended ({:?})", session_id, reason);
        Ok(())
    }

    /// Delete sidecar index
    pub async fn delete(&self, session_id: &str) -> Result<()> {
        let path = self.sidecar_path(session_id);

        if path.exists() {
            fs::remove_file(&path).await?;
            debug!("Deleted sidecar index for session: {}", session_id);
        }

        Ok(())
    }

    /// List all sidecar indices
    pub async fn list_indices(&self) -> Result<Vec<(String, SessionSidecarIndex)>> {
        let mut indices = vec![];

        if !self.storage_dir.exists() {
            return Ok(indices);
        }

        let mut entries = fs::read_dir(&self.storage_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".index.json") {
                    let session_id = name.trim_end_matches(".index.json").to_string();
                    if let Ok(Some(index)) = self.load(&session_id).await {
                        indices.push((session_id, index));
                    }
                }
            }
        }

        // Sort by updated_at descending
        indices.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));

        Ok(indices)
    }

    /// Rebuild sidecar from session events
    ///
    /// This is used to recover the index if it's lost or corrupted.
    pub async fn rebuild_from_events(
        &self,
        session_id: &str,
        events: &[SessionEvent],
        instance_id: &str,
    ) -> Result<SessionSidecarIndex> {
        if events.is_empty() {
            return Err(anyhow::anyhow!("No events to rebuild from"));
        }

        // Get trigger from first event
        let trigger = match &events[0] {
            SessionEvent::SessionCreated(e) => e.trigger.clone(),
            _ => SessionTrigger::User,
        };

        // Get parent session ID from first event
        let parent_session_id = match &events[0] {
            SessionEvent::SessionCreated(e) => e.parent_session_id.clone(),
            _ => None,
        };

        // Create new index
        let mut index = if let Some(parent) = parent_session_id {
            SessionSidecarIndex::new_branched(session_id, instance_id, parent, trigger)
        } else {
            SessionSidecarIndex::new(session_id, instance_id, trigger)
        };

        // Process all events
        for event in events {
            match event {
                SessionEvent::UserMessage(_) => {
                    index.event_count += 1;
                }
                SessionEvent::AssistantMessage(e) => {
                    index.event_count += 1;
                    index.turn_count += 1;
                    index.total_tokens += e.usage.total_tokens;

                    if index.turn_count == 1 && index.title.is_none() {
                        let title = e
                            .content
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(60)
                            .collect::<String>();
                        if !title.is_empty() {
                            index.title = Some(title);
                        }
                    }
                }
                SessionEvent::SessionEnded(_) => {
                    index.event_count += 1;
                    index.ended = true;
                }
                _ => {
                    index.event_count += 1;
                }
            }
        }

        index.updated_at = Utc::now();

        // Save rebuilt index
        self.save(session_id, &index).await?;

        info!("Rebuilt sidecar index for session: {}", session_id);
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::events::{
        AssistantMessageEvent, EventEnvelope, TokenUsage, UserMessageEvent,
    };
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_create_and_load() {
        let temp = TempDir::new().unwrap();
        let manager = SidecarManager::new(temp.path().to_path_buf());

        let index = manager
            .create("sess_123", "inst_456", SessionTrigger::User)
            .await
            .unwrap();

        assert_eq!(index.session_id, "sess_123");
        assert_eq!(index.instance_id, "inst_456");
        assert!(!index.ended);

        let loaded = manager.load("sess_123").await.unwrap().unwrap();
        assert_eq!(loaded.session_id, "sess_123");
    }

    #[tokio::test]
    async fn test_branched_session() {
        let temp = TempDir::new().unwrap();
        let manager = SidecarManager::new(temp.path().to_path_buf());

        let index = manager
            .create_branched(
                "sess_child",
                "inst_456",
                "sess_parent",
                SessionTrigger::Branch,
            )
            .await
            .unwrap();

        assert_eq!(index.parent_session_id, Some("sess_parent".to_string()));
        assert!(matches!(index.trigger, SessionTrigger::Branch));
    }

    #[tokio::test]
    async fn test_auto_title_generation() {
        let temp = TempDir::new().unwrap();
        let manager = SidecarManager::new(temp.path().to_path_buf());

        manager
            .create("sess_123", "inst_456", SessionTrigger::User)
            .await
            .unwrap();

        // Simulate first assistant message
        let event = SessionEvent::AssistantMessage(AssistantMessageEvent {
            envelope: EventEnvelope {
                id: "evt_001".to_string(),
                session_id: "sess_123".to_string(),
                ts: Utc::now(),
                seq: 1,
            },
            message_id: "msg_001".to_string(),
            content: "This is the first response from the assistant. It continues here."
                .to_string(),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
        });

        manager.update_with_event("sess_123", &event).await.unwrap();

        let index = manager.load("sess_123").await.unwrap().unwrap();
        assert_eq!(index.turn_count, 1);
        // Title is auto-generated from first 60 chars of first assistant response
        assert_eq!(
            index.title,
            Some("This is the first response from the assistant. It continues ".to_string())
        );
    }

    #[tokio::test]
    async fn test_set_title() {
        let temp = TempDir::new().unwrap();
        let manager = SidecarManager::new(temp.path().to_path_buf());

        manager
            .create("sess_123", "inst_456", SessionTrigger::User)
            .await
            .unwrap();

        manager.set_title("sess_123", "Custom Title").await.unwrap();

        let index = manager.load("sess_123").await.unwrap().unwrap();
        assert_eq!(index.title, Some("Custom Title".to_string()));
    }

    #[tokio::test]
    async fn test_mark_ended() {
        let temp = TempDir::new().unwrap();
        let manager = SidecarManager::new(temp.path().to_path_buf());

        manager
            .create("sess_123", "inst_456", SessionTrigger::User)
            .await
            .unwrap();

        manager
            .mark_ended(
                "sess_123",
                crate::session::events::SessionEndReason::UserClosed,
            )
            .await
            .unwrap();

        let index = manager.load("sess_123").await.unwrap().unwrap();
        assert!(index.ended);
    }
}
