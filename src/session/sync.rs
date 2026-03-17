//! Session Synchronization Module
//!
//! Coordinates between JSONL storage and sidecar index updates,
//! ensuring consistency between the source of truth (JSONL) and
//! the read-optimized index (.index.json sidecar).

use crate::session::events::SessionEvent;
use crate::session::events::SessionTrigger;
use crate::session::jsonl::SessionStorage;
use crate::session::sidecar::SidecarManager;
use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, error, info};

/// Synchronized session storage
///
/// This combines JSONL storage with sidecar index management,
/// ensuring that every event written to the JSONL is reflected
/// in the sidecar index.
#[derive(Debug, Clone)]
pub struct SyncSessionStorage {
    /// JSONL storage (source of truth)
    jsonl: SessionStorage,
    /// Sidecar index manager
    sidecar: SidecarManager,
}

impl SyncSessionStorage {
    /// Create new synchronized storage
    #[must_use]
    pub fn new(storage_dir: PathBuf) -> Self {
        Self {
            jsonl: SessionStorage::new(storage_dir.clone()),
            sidecar: SidecarManager::new(storage_dir),
        }
    }

    /// Get the underlying JSONL storage
    #[must_use]
    pub fn jsonl(&self) -> &SessionStorage {
        &self.jsonl
    }

    /// Get the sidecar manager
    #[must_use]
    pub fn sidecar(&self) -> &SidecarManager {
        &self.sidecar
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

        // Create sidecar index first
        self.sidecar
            .create(session_id, instance_id, trigger.clone())
            .await?;

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
            trigger,
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
        instance_id: &str,
        parent_session_id: &str,
        cwd: Option<String>,
    ) -> Result<()> {
        // Copy parent session file
        self.jsonl
            .copy_session(parent_session_id, session_id)
            .await?;

        // Create branched sidecar
        self.sidecar
            .create_branched(
                session_id,
                instance_id,
                parent_session_id,
                SessionTrigger::Branch,
            )
            .await?;

        info!(
            "Created branched session: {} from {}",
            session_id, parent_session_id
        );
        Ok(())
    }

    /// Append an event to both JSONL and sidecar
    ///
    /// This ensures the sidecar stays in sync with the JSONL.
    pub async fn append_event(&self, session_id: &str, event: &SessionEvent) -> Result<()> {
        // Write to JSONL first (source of truth)
        self.jsonl.append_event(session_id, event).await?;

        // Update sidecar
        if let Err(e) = self.sidecar.update_with_event(session_id, event).await {
            error!(
                "Failed to update sidecar for session {}: {}. JSONL was written successfully.",
                session_id, e
            );
            // Don't fail the operation - sidecar can be rebuilt from JSONL
        }

        Ok(())
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

        // Mark sidecar as ended
        self.sidecar.mark_ended(session_id, reason).await?;

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

    /// Load sidecar index
    pub async fn load_index(
        &self,
        session_id: &str,
    ) -> Result<Option<crate::session::sidecar::SessionSidecarIndex>> {
        self.sidecar.load(session_id).await
    }

    /// Set session title
    pub async fn set_title(&self, session_id: &str, title: impl Into<String>) -> Result<()> {
        self.sidecar.set_title(session_id, title).await
    }

    /// Check if session exists
    pub async fn session_exists(&self, session_id: &str) -> bool {
        self.jsonl.session_exists(session_id).await
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> Result<Vec<String>> {
        self.jsonl.list_sessions().await
    }

    /// Delete a session (both JSONL and sidecar)
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.jsonl.delete_session(session_id).await?;
        self.sidecar.delete(session_id).await?;

        info!("Deleted synchronized session: {}", session_id);
        Ok(())
    }

    /// Recover/rebuild sidecar from JSONL
    ///
    /// This is used when the sidecar is lost or corrupted.
    pub async fn recover_sidecar(&self, session_id: &str, instance_id: &str) -> Result<()> {
        // Load all events from JSONL
        let events = self.jsonl.load_events(session_id).await?;

        if events.is_empty() {
            return Err(anyhow::anyhow!(
                "No events found for session {}",
                session_id
            ));
        }

        // Rebuild sidecar
        self.sidecar
            .rebuild_from_events(session_id, &events, instance_id)
            .await?;

        info!("Recovered sidecar for session: {}", session_id);
        Ok(())
    }

    /// Verify and repair sidecar if needed
    ///
    /// Checks if sidecar is in sync with JSONL and repairs if not.
    pub async fn verify_and_repair(&self, session_id: &str, instance_id: &str) -> Result<bool> {
        // Check if sidecar exists
        let sidecar_exists = self.sidecar.exists(session_id).await;

        if !sidecar_exists {
            debug!("Sidecar missing for session {}, rebuilding", session_id);
            self.recover_sidecar(session_id, instance_id).await?;
            return Ok(true);
        }

        // Load both
        let events = self.jsonl.load_events(session_id).await?;
        let index = self.sidecar.load(session_id).await?;

        if let Some(index) = index {
            // Check event count
            let jsonl_event_count = events.len() as u64;
            if index.event_count != jsonl_event_count {
                debug!(
                    "Sidecar out of sync for session {}: sidecar={}, jsonl={}",
                    session_id, index.event_count, jsonl_event_count
                );
                self.recover_sidecar(session_id, instance_id).await?;
                return Ok(true);
            }
        }

        Ok(false)
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

        let index = storage.load_index("sess_123").await.unwrap().unwrap();
        assert_eq!(index.session_id, "sess_123");
        assert_eq!(index.instance_id, "inst_456");
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

        // Verify sidecar updated
        let index = storage.load_index("sess_123").await.unwrap().unwrap();
        assert_eq!(index.event_count, 2);
        assert_eq!(index.turn_count, 1);
        assert_eq!(index.total_tokens, 15);
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

        let index = storage.load_index("sess_123").await.unwrap().unwrap();
        assert!(index.ended);
    }

    #[tokio::test]
    async fn test_recover_sidecar() {
        let temp = TempDir::new().unwrap();
        let storage = SyncSessionStorage::new(temp.path().to_path_buf());

        // Create session with events
        storage
            .create_session("sess_123", "inst_456", SessionTrigger::User, None)
            .await
            .unwrap();

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

        // Delete sidecar
        storage.sidecar.delete("sess_123").await.unwrap();
        assert!(!storage.sidecar.exists("sess_123").await);

        // Recover - note: we have 2 events: session.created and user.message
        storage
            .recover_sidecar("sess_123", "inst_456")
            .await
            .unwrap();

        // Verify
        assert!(storage.sidecar.exists("sess_123").await);
        let index = storage.load_index("sess_123").await.unwrap().unwrap();
        // 2 events: session.created and user.message
        assert_eq!(index.event_count, 2);
    }
}
