//! Session Synchronization Module
//!
//! Coordinates JSONL storage, ensuring consistency for the source of truth.

use crate::session::events::SessionEvent;
use crate::session::events::SessionTrigger;
use crate::session::jsonl::SessionStorage;
use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, info};

/// Synchronized session storage
///
/// This provides JSONL storage as the source of truth.
#[derive(Debug, Clone)]
pub struct SyncSessionStorage {
    /// JSONL storage (source of truth)
    jsonl: SessionStorage,
}

impl SyncSessionStorage {
    /// Create new synchronized storage
    #[must_use]
    pub fn new(storage_dir: PathBuf) -> Self {
        Self {
            jsonl: SessionStorage::new(storage_dir),
        }
    }

    /// Get the underlying JSONL storage
    #[must_use]
    pub fn jsonl(&self) -> &SessionStorage {
        &self.jsonl
    }

    /// Create a new session with both JSONL and sidecar
    pub async fn create_session(
        &self,
        session_id: &str,
        instance_id: &str,
        trigger: SessionTrigger,
        _cwd: Option<String>,
    ) -> Result<()> {
        use crate::session::events::{EventEnvelope, SessionCreatedEvent};
        use chrono::Utc;
        use tokio::fs;
        use tokio::io::AsyncWriteExt;

        // Ensure directory exists
        fs::create_dir_all(&self.jsonl.storage_dir()).await?;

        // Create the session.created event
        let created_event = SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: "evt_001".to_string(),
                session_id: session_id.to_string(),
                ts: Utc::now(),
                seq: 1,
            },
            instance_id: instance_id.to_string(),
            image_digest: String::new(), // Will be set when instance starts
            parent_session_id: None,
            trigger: trigger.clone(),
        });

        // Write the initial event atomically
        let path = self.jsonl.storage_dir().join(format!("{session_id}.jsonl"));
        let json = serde_json::to_string(&created_event)?;

        let temp_path = path.with_extension("tmp");
        {
            let mut file = fs::File::create(&temp_path).await?;
            file.write_all((json + "\n").as_bytes()).await?;
            file.flush().await?;
        }
        fs::rename(&temp_path, &path).await?;

        info!("Created synchronized session: {}", session_id);
        Ok(())
    }

    /// Create a branched session
    pub async fn create_branched_session(
        &self,
        session_id: &str,
        _instance_id: &str,
        parent_session_id: &str,
        _cwd: Option<String>,
    ) -> Result<()> {
        // Copy parent session file
        self.jsonl
            .copy_session(parent_session_id, session_id)
            .await?;

        info!(
            "Created branched session: {} from {}",
            session_id, parent_session_id
        );
        Ok(())
    }

    /// Append an event to JSONL
    pub async fn append_event(&self, session_id: &str, event: &SessionEvent) -> Result<()> {
        self.jsonl.append_event(session_id, event).await
    }

    /// End a session
    ///
    /// Writes session.ended event and marks the sidecar as ended.
    pub async fn end_session(
        &self,
        session_id: &str,
        reason: crate::session::events::SessionEndReason,
        turn_count: u32,
        total_tokens: u32,
    ) -> Result<()> {
        use crate::session::events::{EventEnvelope, SessionEndedEvent};
        use chrono::Utc;

        // Create ended event
        let event = SessionEvent::SessionEnded(SessionEndedEvent {
            envelope: EventEnvelope {
                id: format!(
                    "evt_{:03}",
                    self.get_next_seq(session_id).await.unwrap_or(1)
                ),
                session_id: session_id.to_string(),
                ts: Utc::now(),
                seq: 0, // Will be determined by append
            },
            reason: reason.clone(),
            turn_count,
            total_tokens,
        });

        // Append event
        self.append_event(session_id, &event).await?;

        info!("Ended session: {}", session_id);
        Ok(())
    }

    /// Get the next sequence number for a session
    async fn get_next_seq(&self, session_id: &str) -> Result<u64> {
        let events = self.jsonl.load_events(session_id).await?;
        Ok(events.len() as u64 + 1)
    }

    /// Load session events
    pub async fn load_events(&self, session_id: &str) -> Result<Vec<SessionEvent>> {
        self.jsonl.load_events(session_id).await
    }

    /// Check if session exists
    pub async fn session_exists(&self, session_id: &str) -> bool {
        self.jsonl.session_exists(session_id).await
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> Result<Vec<String>> {
        self.jsonl.list_sessions().await
    }

    /// Delete a session
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.jsonl.delete_session(session_id).await?;

        info!("Deleted synchronized session: {}", session_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::events::{
        AssistantMessageEvent, EventEnvelope, TokenUsage, UserMessageEvent,
    };
    use chrono::Utc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_create_session() {
        let temp = TempDir::new().unwrap();
        let storage = SyncSessionStorage::new(temp.path().to_path_buf());

        storage
            .create_session(
                "sess_123",
                "inst_456",
                SessionTrigger::User,
                Some("/workspace".to_string()),
            )
            .await
            .unwrap();

        assert!(storage.session_exists("sess_123").await);
    }

    #[tokio::test]
    async fn test_append_event_sync() {
        let temp = TempDir::new().unwrap();
        let storage = SyncSessionStorage::new(temp.path().to_path_buf());

        storage
            .create_session("sess_123", "inst_456", SessionTrigger::User, None)
            .await
            .unwrap();

        // Append user message
        let user_event = SessionEvent::UserMessage(UserMessageEvent {
            envelope: EventEnvelope {
                id: "evt_001".to_string(),
                session_id: "sess_123".to_string(),
                ts: Utc::now(),
                seq: 1,
            },
            message_id: "msg_001".to_string(),
            content: "Hello".to_string(),
            source: crate::session::events::MessageSource::User,
        });

        storage.append_event("sess_123", &user_event).await.unwrap();

        // Append assistant message
        let assistant_event = SessionEvent::AssistantMessage(AssistantMessageEvent {
            envelope: EventEnvelope {
                id: "evt_002".to_string(),
                session_id: "sess_123".to_string(),
                ts: Utc::now(),
                seq: 2,
            },
            message_id: "msg_002".to_string(),
            content: "Hi there!".to_string(),
            usage: TokenUsage {
                input_tokens: 5,
                output_tokens: 10,
                total_tokens: 15,
            },
        });

        storage
            .append_event("sess_123", &assistant_event)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_end_session() {
        let temp = TempDir::new().unwrap();
        let storage = SyncSessionStorage::new(temp.path().to_path_buf());

        storage
            .create_session("sess_123", "inst_456", SessionTrigger::User, None)
            .await
            .unwrap();

        storage
            .end_session(
                "sess_123",
                crate::session::events::SessionEndReason::UserClosed,
                5,
                100,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_list_sessions() {
        let temp = TempDir::new().unwrap();
        let storage = SyncSessionStorage::new(temp.path().to_path_buf());

        // Create multiple sessions
        storage
            .create_session("sess_1", "inst_001", SessionTrigger::User, None)
            .await
            .unwrap();
        storage
            .create_session("sess_2", "inst_001", SessionTrigger::User, None)
            .await
            .unwrap();

        let sessions = storage.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"sess_1".to_string()));
        assert!(sessions.contains(&"sess_2".to_string()));
    }

    #[tokio::test]
    async fn test_delete_session() {
        let temp = TempDir::new().unwrap();
        let storage = SyncSessionStorage::new(temp.path().to_path_buf());

        storage
            .create_session("sess_delete", "inst_001", SessionTrigger::User, None)
            .await
            .unwrap();

        assert!(storage.session_exists("sess_delete").await);

        storage.delete_session("sess_delete").await.unwrap();

        assert!(!storage.session_exists("sess_delete").await);
    }

    #[tokio::test]
    async fn test_create_branched_session() {
        let temp = TempDir::new().unwrap();
        let storage = SyncSessionStorage::new(temp.path().to_path_buf());

        // Create parent session
        storage
            .create_session("parent_sess", "inst_001", SessionTrigger::User, None)
            .await
            .unwrap();

        let user_event = SessionEvent::UserMessage(UserMessageEvent {
            envelope: EventEnvelope {
                id: "evt_001".to_string(),
                session_id: "parent_sess".to_string(),
                ts: Utc::now(),
                seq: 2,
            },
            message_id: "msg_001".to_string(),
            content: "Hello".to_string(),
            source: crate::session::events::MessageSource::User,
        });
        storage
            .append_event("parent_sess", &user_event)
            .await
            .unwrap();

        // Create branched session
        storage
            .create_branched_session("child_sess", "inst_001", "parent_sess", None)
            .await
            .unwrap();

        // Both sessions should exist
        assert!(storage.session_exists("parent_sess").await);
        assert!(storage.session_exists("child_sess").await);

        // Child should have copied events
        let child_events = storage.load_events("child_sess").await.unwrap();
        assert_eq!(child_events.len(), 2); // session.created + user.message
    }
}
