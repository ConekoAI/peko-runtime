//! Base session implementation
//!
//! The `BaseSession` provides shared conversation context that is accessible
//! across all overlays for a given peer. It stores:
//! - Conversation history (messages)
//! - Token usage
//! - Current provider/model settings
//! - Session metadata

use super::derive_base_session_key;
use super::index::{IndexEntry, SessionIndex};
use super::jsonl::SessionStorage;
use super::types::Peer;
use crate::providers::ChatMessage;
use crate::types::ContentBlock;
use anyhow::{Context, Result};
use chrono::Utc;
use std::path::PathBuf;
use tokio::fs;

/// Base session shared across all overlays for a peer
///
/// The `BaseSession` maintains the core conversation history and metadata
/// that is shared between all channel overlays and spawn overlays
/// (for non-isolated spawns).
#[derive(Debug)]
pub struct BaseSession {
    /// Session ID (unique identifier)
    pub id: String,
    /// Agent name
    pub agent_name: String,
    /// Base session key: agent:{agent}:peer:{type}:{id}
    pub session_key: String,
    /// The peer this session belongs to
    pub peer: Peer,
    /// Storage backend
    storage: SessionStorage,
    /// Session index for metadata
    index: SessionIndex,
    /// Last message ID (for chaining)
    last_message_id: Option<String>,
    /// Message count
    pub message_count: usize,
    /// Input tokens
    pub input_tokens: usize,
    /// Output tokens
    pub output_tokens: usize,
    /// Current provider
    pub current_provider: Option<String>,
    /// Current model
    pub current_model: Option<String>,
}

impl BaseSession {
    /// Get the storage directory for an agent
    #[must_use]
    pub fn storage_dir(agent_name: &str, team: Option<&str>) -> PathBuf {
        let team = team.unwrap_or("default");
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("teams")
            .join(team)
            .join("agents")
            .join(agent_name)
            .join("sessions")
    }

    /// Create a new base session for an agent and peer
    ///
    /// # Arguments
    /// * `agent_name` - The agent name
    /// * `peer` - The peer this session belongs to
    pub async fn create(agent_name: &str, peer: &Peer) -> Result<Self> {
        let session_key = derive_base_session_key(agent_name, peer);
        let session_id = format!(
            "{}_{}_{}",
            agent_name,
            peer.peer_type(),
            Utc::now().timestamp_millis()
        );

        Self::create_with_key(agent_name, peer, &session_id, &session_key).await
    }

    /// Create a new base session with specific ID and key
    pub async fn create_with_key(
        agent_name: &str,
        peer: &Peer,
        session_id: &str,
        session_key: &str,
    ) -> Result<Self> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let storage = SessionStorage::new(storage_dir.clone());
        let mut index = SessionIndex::open(&storage_dir);

        // Ensure index is migrated
        index
            .migrate_from_directory(agent_name)
            .await
            .with_context(|| format!("Failed to migrate index for agent: {agent_name}"))?;

        // Create session file
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());

        storage
            .create_session(session_id, cwd.clone())
            .await
            .with_context(|| {
                format!(
                    "Failed to create session file: {}/{}",
                    storage_dir.display(),
                    session_id
                )
            })?;

        // Create index entry
        let transcript_file = format!("{session_id}.jsonl");
        let mut entry = IndexEntry::new(
            session_id.to_string(),
            agent_name.to_string(),
            transcript_file,
        );
        entry.session_key = Some(session_key.to_string());
        entry.cwd = cwd;

        // Insert into index
        index
            .insert(session_key.to_string(), entry)
            .await
            .with_context(|| "Failed to insert into index")?;

        Ok(Self {
            id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            session_key: session_key.to_string(),
            peer: peer.clone(),
            storage,
            index,
            last_message_id: None,
            message_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            current_provider: None,
            current_model: None,
        })
    }

    /// Open an existing base session by agent and peer
    pub async fn open(agent_name: &str, peer: &Peer) -> Result<Option<Self>> {
        let session_key = derive_base_session_key(agent_name, peer);
        Self::open_by_key(agent_name, &session_key).await
    }

    /// Open an existing base session by key
    pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let storage = SessionStorage::new(storage_dir.clone());
        let mut index = SessionIndex::open(&storage_dir);

        // Ensure index is migrated
        index.migrate_from_directory(agent_name).await?;

        // Look up in index
        let entry = match index.get(session_key).await? {
            Some(e) => e,
            None => return Ok(None),
        };

        // Load session entries to find last message
        let entries: Vec<super::SessionEntry> = storage.load_session(&entry.session_id).await?;

        if entries.is_empty() {
            return Ok(None);
        }

        // Count messages and find last ID
        let mut message_count = 0;
        let last_message_id = entries.iter().rev().find_map(|entry| match entry {
            super::SessionEntry::Message { id, .. } => {
                message_count += 1;
                Some(id.clone())
            }
            _ => None,
        });

        // Parse peer from session key
        let peer = parse_peer_from_key(session_key)?;

        // Get token counts from index
        let input_tokens = entry.input_tokens.unwrap_or(0);
        let output_tokens = entry.output_tokens.unwrap_or(0);

        Ok(Some(Self {
            id: entry.session_id.clone(),
            agent_name: agent_name.to_string(),
            session_key: session_key.to_string(),
            peer,
            storage,
            index,
            last_message_id,
            message_count,
            input_tokens,
            output_tokens,
            current_provider: entry.provider.clone(),
            current_model: entry.model.clone(),
        }))
    }

    /// Open a session by ID from a specific directory (registry-based)
    ///
    /// This bypasses the index and opens the session file directly.
    pub async fn open_by_id(
        agent_name: &str,
        peer: &Peer,
        session_id: &str,
        sessions_dir: impl AsRef<std::path::Path>,
    ) -> Result<Self> {
        let sessions_dir = sessions_dir.as_ref().to_path_buf();
        let storage = SessionStorage::new(sessions_dir.clone());
        let index = SessionIndex::open(sessions_dir);
        let session_key = derive_base_session_key(agent_name, peer);

        // Load session entries
        let entries: Vec<super::SessionEntry> = storage.load_session(session_id).await?;

        // Count messages and find last ID
        let mut message_count = 0;
        let last_message_id = entries.iter().rev().find_map(|entry| match entry {
            super::SessionEntry::Message { id, .. } => {
                message_count += 1;
                Some(id.clone())
            }
            _ => None,
        });

        Ok(Self {
            id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            session_key,
            peer: peer.clone(),
            storage,
            index,
            last_message_id,
            message_count,
            input_tokens: 0,
            output_tokens: 0,
            current_provider: None,
            current_model: None,
        })
    }

    /// Create a session with specific path (registry-based)
    ///
    /// Creates a session file in the specified directory with the given session ID.
    pub async fn create_with_path(
        agent_name: &str,
        peer: &Peer,
        session_id: &str,
        sessions_dir: impl AsRef<std::path::Path>,
    ) -> Result<Self> {
        let sessions_dir = sessions_dir.as_ref().to_path_buf();
        let storage = SessionStorage::new(sessions_dir.clone());
        let index = SessionIndex::open(&sessions_dir);
        let session_key = derive_base_session_key(agent_name, peer);

        // Ensure directory exists
        fs::create_dir_all(&sessions_dir).await?;

        // Create session file
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());

        storage.create_session(session_id, cwd).await?;

        Ok(Self {
            id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            session_key,
            peer: peer.clone(),
            storage,
            index,
            last_message_id: None,
            message_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            current_provider: None,
            current_model: None,
        })
    }

    /// Get or create a base session
    pub async fn get_or_create(agent_name: &str, peer: &Peer) -> Result<Self> {
        match Self::open(agent_name, peer).await? {
            Some(session) => Ok(session),
            None => Self::create(agent_name, peer).await,
        }
    }

    /// Update the index with current metadata
    async fn update_index(&mut self) -> Result<()> {
        if let Some(mut entry) = self.index.get(&self.session_key).await? {
            entry.touch();
            entry.message_count = self.message_count;
            entry.input_tokens = Some(self.input_tokens);
            entry.output_tokens = Some(self.output_tokens);
            entry.total_tokens = Some(self.input_tokens + self.output_tokens);
            entry.provider = self.current_provider.clone();
            entry.model = self.current_model.clone();
            self.index.insert(self.session_key.clone(), entry).await?;
        }
        Ok(())
    }

    /// Record token usage
    pub async fn record_usage(&mut self, input: usize, output: usize) -> Result<()> {
        self.input_tokens += input;
        self.output_tokens += output;
        self.update_index().await
    }

    /// Set the current model
    pub async fn set_model(&mut self, provider: &str, model: &str) -> Result<()> {
        self.current_provider = Some(provider.to_string());
        self.current_model = Some(model.to_string());
        self.update_index().await
    }

    /// Get token usage
    #[must_use]
    pub fn token_usage(&self) -> (usize, usize, usize) {
        (
            self.input_tokens,
            self.output_tokens,
            self.input_tokens + self.output_tokens,
        )
    }

    /// Add a system message
    pub async fn add_system(&mut self, content: impl Into<String>) -> Result<()> {
        let msg_id = self
            .storage
            .append_message(
                &self.id,
                self.last_message_id.clone(),
                "system",
                vec![ContentBlock::Text {
                    text: content.into(),
                }],
            )
            .await?;
        self.last_message_id = Some(msg_id);
        self.message_count += 1;
        self.update_index().await?;
        Ok(())
    }

    /// Add a user message
    pub async fn add_user(&mut self, content: impl Into<String>) -> Result<()> {
        let msg_id = self
            .storage
            .append_message(
                &self.id,
                self.last_message_id.clone(),
                "user",
                vec![ContentBlock::Text {
                    text: content.into(),
                }],
            )
            .await?;
        self.last_message_id = Some(msg_id);
        self.message_count += 1;
        self.update_index().await?;
        Ok(())
    }

    /// Add an assistant message with optional tool calls
    pub async fn add_assistant(
        &mut self,
        content: impl Into<String>,
        tool_calls: Option<Vec<crate::engine::ToolCall>>,
    ) -> Result<()> {
        let content_str = content.into();
        let content_blocks = if let Some(calls) = tool_calls {
            let mut blocks = vec![];
            if !content_str.is_empty() {
                blocks.push(ContentBlock::Text { text: content_str });
            }
            for (idx, call) in calls.iter().enumerate() {
                blocks.push(ContentBlock::ToolCall {
                    id: format!("call_{}_{}", self.id, idx),
                    name: call.name.clone(),
                    arguments: call.parameters.clone(),
                });
            }
            blocks
        } else {
            vec![ContentBlock::Text { text: content_str }]
        };

        let msg_id = self
            .storage
            .append_message(
                &self.id,
                self.last_message_id.clone(),
                "assistant",
                content_blocks,
            )
            .await?;
        self.last_message_id = Some(msg_id);
        self.message_count += 1;
        self.update_index().await?;
        Ok(())
    }

    /// Add a tool result
    pub async fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        result: impl Into<String>,
    ) -> Result<()> {
        self.storage
            .append_tool_result(
                &self.id,
                &tool_call_id.into(),
                &tool_name.into(),
                result.into(),
                false,
            )
            .await?;
        Ok(())
    }

    /// Load conversation history
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> {
        use crate::providers::MessageRole;

        let entries = self.storage.load_session(&self.id).await?;
        let mut messages = Vec::new();

        for entry in entries {
            match entry {
                super::SessionEntry::Message { message, .. } => {
                    let role = match message.role.as_str() {
                        "system" => MessageRole::System,
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        "tool" => MessageRole::Tool,
                        _ => continue,
                    };

                    messages.push(ChatMessage {
                        role,
                        content: message.content,
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                super::SessionEntry::ToolResult {
                    tool_call_id,
                    content,
                    ..
                } => {
                    let result_text: String = content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();

                    messages.push(ChatMessage {
                        role: MessageRole::Tool,
                        content: vec![ContentBlock::Text { text: result_text }],
                        tool_calls: None,
                        tool_call_id: Some(tool_call_id),
                    });
                }
                _ => {}
            }
        }

        Ok(messages)
    }

    /// Record a compaction event
    pub async fn record_compaction(
        &mut self,
        summary: &str,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
    ) -> Result<()> {
        self.storage
            .append_compaction(
                &self.id,
                self.last_message_id.clone(),
                summary,
                messages_compacted,
                tokens_before,
                tokens_after,
                compaction_number,
            )
            .await?;
        Ok(())
    }

    /// Load the most recent compaction summary
    pub async fn load_previous_compaction_summary(&self) -> Result<Option<String>> {
        let entries: Vec<super::SessionEntry> = self.storage.load_session(&self.id).await?;

        for entry in entries.iter().rev() {
            if let super::SessionEntry::Compaction { summary, .. } = entry {
                return Ok(Some(summary.clone()));
            }
        }

        Ok(None)
    }

    /// Get context as text (for debugging/display)
    pub async fn get_context_text(&self, _limit: usize) -> String {
        let entries = match self.storage.load_session(&self.id).await {
            Ok(e) => e,
            Err(_) => return format!("Session: {}", self.id),
        };

        let mut context = String::new();

        for entry in entries {
            match entry {
                super::SessionEntry::Message { message, .. } => {
                    let role = &message.role;
                    let mut parts: Vec<String> = Vec::new();

                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text } => parts.push(text.clone()),
                            ContentBlock::Thinking { text, .. } => parts.push(text.clone()),
                            ContentBlock::ToolCall {
                                name, arguments, ..
                            } => {
                                let args_str =
                                    serde_json::to_string(&arguments).unwrap_or_default();
                                parts.push(format!("[ToolCall: {name}({args_str})]"));
                            }
                            ContentBlock::ToolResult { content, .. } => {
                                let result_text: String = content
                                    .iter()
                                    .filter_map(|c| match c {
                                        ContentBlock::Text { text } => Some(text.clone()),
                                        _ => None,
                                    })
                                    .collect();
                                parts.push(format!("[ToolResult: {result_text}]"));
                            }
                            _ => {}
                        }
                    }

                    let content_text = parts.join("\n");
                    if !content_text.is_empty() {
                        context.push_str(&format!("{role}: {content_text}\n\n"));
                    }
                }
                super::SessionEntry::ToolResult {
                    tool_name, content, ..
                } => {
                    let result_text: String = content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    context.push_str(&format!("tool: [{tool_name} result: {result_text}]\n\n"));
                }
                _ => {}
            }
        }

        if context.is_empty() {
            format!("Session: {}", self.id)
        } else {
            context
        }
    }
}

/// Parse a peer from a base session key
fn parse_peer_from_key(key: &str) -> Result<Peer> {
    // Format: agent:{agent}:peer:{type}:{id}
    let parts: Vec<&str> = key.split(':').collect();

    if parts.len() < 5 {
        return Err(anyhow::anyhow!("Invalid base session key format: {key}"));
    }

    // Find "peer" in the key
    if let Some(peer_idx) = parts.iter().position(|&p| p == "peer") {
        let peer_type = parts.get(peer_idx + 1).unwrap_or(&"user");
        let peer_id_parts: Vec<_> = parts
            .iter()
            .skip(peer_idx + 2)
            .take_while(|&&p| p != "overlay")
            .copied()
            .collect();
        let peer_id = peer_id_parts.join(":");

        match *peer_type {
            "agent" => Ok(Peer::Agent(peer_id)),
            _ => Ok(Peer::User(peer_id)),
        }
    } else {
        // Legacy format fallback
        Ok(Peer::User("default".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_base_session_create() {
        let peer = Peer::User("alice".to_string());
        let session = BaseSession::create("test_agent", &peer).await;
        assert!(session.is_ok());

        let session = session.unwrap();
        assert_eq!(session.agent_name, "test_agent");
        assert_eq!(session.peer, peer);
        assert!(session.session_key.contains("peer:user:alice"));
        assert_eq!(session.message_count, 0);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_base_session_agent_peer() {
        let peer = Peer::Agent("helper".to_string());
        let session = BaseSession::create("test_agent", &peer).await.unwrap();

        assert_eq!(session.peer, peer);
        assert!(session.session_key.contains("peer:agent:helper"));
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_base_session_add_messages() {
        let peer = Peer::User("alice".to_string());
        let mut session = BaseSession::create("test_agent", &peer).await.unwrap();

        session
            .add_system("You are a helpful assistant")
            .await
            .unwrap();
        session.add_user("Hello!").await.unwrap();
        session.add_assistant("Hi there!", None).await.unwrap();

        assert_eq!(session.message_count, 3);

        let history = session.load_history().await.unwrap();
        assert_eq!(history.len(), 3);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_base_session_token_usage() {
        let peer = Peer::User("alice".to_string());
        let mut session = BaseSession::create("test_agent", &peer).await.unwrap();

        session.record_usage(100, 50).await.unwrap();
        session.record_usage(50, 25).await.unwrap();

        let (input, output, total) = session.token_usage();
        assert_eq!(input, 150);
        assert_eq!(output, 75);
        assert_eq!(total, 225);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_base_session_persistence() {
        let peer = Peer::User("alice".to_string());

        // Create session
        let mut session = BaseSession::create("test_agent", &peer).await.unwrap();
        let session_key = session.session_key.clone();

        session.add_user("Hello!").await.unwrap();
        session.add_assistant("Hi!", None).await.unwrap();

        // Re-open by key
        let reopened = BaseSession::open_by_key("test_agent", &session_key)
            .await
            .unwrap();

        assert!(reopened.is_some());
        let reopened = reopened.unwrap();
        assert_eq!(reopened.session_key, session_key);
        assert_eq!(reopened.message_count, 2);

        let history = reopened.load_history().await.unwrap();
        assert_eq!(history.len(), 2);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_base_session_get_or_create() {
        let peer = Peer::User("alice".to_string());

        // Create new
        let session1 = BaseSession::get_or_create("test_agent", &peer)
            .await
            .unwrap();
        let key1 = session1.session_key.clone();

        // Get existing
        let session2 = BaseSession::get_or_create("test_agent", &peer)
            .await
            .unwrap();
        let key2 = session2.session_key;

        assert_eq!(key1, key2);
    }

    #[test]
    fn test_parse_peer_from_key() {
        // User peer
        let peer = parse_peer_from_key("agent:test:peer:user:alice").unwrap();
        assert_eq!(peer, Peer::User("alice".to_string()));

        // Agent peer
        let peer = parse_peer_from_key("agent:test:peer:agent:helper").unwrap();
        assert_eq!(peer, Peer::Agent("helper".to_string()));

        // Complex user ID with colons (sanitized)
        let peer = parse_peer_from_key("agent:test:peer:user:domain_user_123").unwrap();
        assert_eq!(peer, Peer::User("domain_user_123".to_string()));
    }

    #[test]
    fn test_parse_peer_from_key_invalid() {
        let result = parse_peer_from_key("invalid_key");
        assert!(result.is_err());
    }
}
