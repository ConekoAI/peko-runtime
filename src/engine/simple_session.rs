//! Simple session persistence wrapper
//!
//! Wraps the existing transcript module for mandatory session persistence.

use crate::session::transcript::{TranscriptConfig, TranscriptEntry, TranscriptStorage};
use crate::engine::ToolCall;
use anyhow::Result;
use std::path::PathBuf;

/// Simple session that auto-persists to transcript
pub struct SimpleSession {
    /// Session ID
    pub id: String,
    /// Storage
    storage: TranscriptStorage,
    /// Entries in memory
    entries: Vec<TranscriptEntry>,
}

impl SimpleSession {
    /// Create a new session for an agent
    pub async fn create(agent_name: &str) -> Result<Self> {
        let session_id = format!("{}_{}", agent_name, chrono::Utc::now().timestamp_millis());
        
        // Use agent-specific session directory
        let storage_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("agents")
            .join(agent_name)
            .join("sessions");
        
        let config = TranscriptConfig {
            storage_dir,
            auto_create: true,
        };
        let storage = TranscriptStorage::new(config);
        
        Ok(Self {
            id: session_id,
            storage,
            entries: Vec::new(),
        })
    }
    
    /// Add system message
    pub async fn add_system(
        &mut self,
        content: impl Into<String>,
    ) -> Result<()> {
        let entry = TranscriptEntry::system(&content.into());
        self.storage.append(&self.id, &entry).await?;
        self.entries.push(entry);
        Ok(())
    }
    
    /// Add user message
    pub async fn add_user(
        &mut self,
        content: impl Into<String>,
    ) -> Result<()> {
        let entry = TranscriptEntry::user(&content.into());
        self.storage.append(&self.id, &entry).await?;
        self.entries.push(entry);
        Ok(())
    }
    
    /// Add assistant message
    pub async fn add_assistant(
        &mut self,
        content: impl Into<String>,
        _tool_calls: Option<Vec<ToolCall>>,
    ) -> Result<()> {
        let entry = TranscriptEntry::assistant(&content.into());
        // Note: tool_calls conversion skipped for now due to type mismatch
        self.storage.append(&self.id, &entry).await?;
        self.entries.push(entry);
        Ok(())
    }
    
    /// Add tool result
    pub async fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        result: impl Into<String>,
    ) -> Result<()> {
        // Build tool result entry manually
        let entry = TranscriptEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            role: "tool".to_string(),
            content: result.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            metadata: None,
        };
        self.storage.append(&self.id, &entry).await?;
        self.entries.push(entry);
        Ok(())
    }
    
    /// Get context as formatted text (last N entries)
    pub fn get_context_text(
        &self,
        max_entries: usize,
    ) -> String {
        self.entries
            .iter()
            .rev()
            .take(max_entries)
            .rev()
            .map(|e| format!("{}: {}", e.role, e.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    
    /// Get session file path
    pub fn path(&self) -> PathBuf {
        self.storage.transcript_path(&self.id)
    }
}
