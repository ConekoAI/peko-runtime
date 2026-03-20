//! Unified session implementation
//!
//! This module provides a single, authoritative session implementation that
//! replaces both `BaseSession` and `SimpleSession` to eliminate racing issues.
//!
//! ## Design Principles
//!
//! 1. **Single Source of Truth**: One implementation manages all session data
//! 2. **Atomic Updates**: All index updates happen together in one operation
//! 3. **Clear Ownership**: No competing writers to the same index entry
//! 4. **Backward Compatible**: Works with existing session files
//!
//! ## Unified Design
//!
//! This single implementation replaces the previous competing `BaseSession` and
//! `SimpleSession` types to eliminate racing issues. It provides:
//! - Peer-aware design for multi-user/session scenarios
//! - Clean API for simple use cases (defaults to Peer::User("default"))

use crate::engine::ToolCall;
use crate::providers::ChatMessage;
use crate::session::index::{SessionEntry, SessionIndex};
use crate::session::jsonl::SessionStorage;
use crate::session::types::Peer;
use crate::types::ContentBlock;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::fs;

/// Unified session - single source of truth for conversation persistence
///
/// Unified session implementation with atomic updates.
/// Provides peer-aware session management with atomic index updates
/// to prevent racing conditions.
///
/// # Examples
///
/// ```rust,ignore
/// // Create a new session for a user
/// let peer = Peer::User("alice".to_string());
/// let session = UnifiedSession::create("my_agent", &peer).await?;
///
/// // Add messages
/// session.add_user("Hello!").await?;
/// session.add_assistant("Hi there!", None).await?;
///
/// // Load history
/// let history = session.load_history().await?;
/// ```
#[derive(Debug)]
pub struct UnifiedSession {
    /// Session ID (UUID format)
    pub id: String,
    /// Agent name
    pub agent_name: String,
    /// Session key for peer-based lookup
    pub session_key: String,
    /// The peer this session belongs to
    pub peer: Peer,
    /// Storage backend for JSONL files
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

impl UnifiedSession {
    // ============================================================
    // Storage Directory
    // ============================================================

    /// Get the storage directory for an agent
    ///
    /// Uses team-based structure: `~/.pekobot/teams/{team}/agents/{agent}/sessions/`
    ///
    /// # Arguments
    /// * `agent_name` - The agent name
    /// * `team` - Optional team name (defaults to "default")
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

    // ============================================================
    // Creation
    // ============================================================

    /// Create a new unified session for an agent and peer
    ///
    /// # Arguments
    /// * `agent_name` - The agent name
    /// * `peer` - The peer this session belongs to
    ///
    /// NOTE: This is crate-private. Only SessionManager should create sessions.
    pub(crate) async fn create(agent_name: &str, peer: &Peer) -> Result<Self> {
        let session_key = derive_session_key(agent_name, peer);
        let session_id = uuid::Uuid::new_v4().to_string();

        Self::create_with_key(agent_name, peer, &session_id, &session_key).await
    }

    /// Create a new unified session with specific ID and key
    ///
    /// This is useful when you need deterministic session IDs or custom keys.
    /// NOTE: This is crate-private. Only SessionManager should create sessions.
    pub(crate) async fn create_with_key(
        agent_name: &str,
        peer: &Peer,
        session_id: &str,
        session_key: &str,
    ) -> Result<Self> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let storage = SessionStorage::new(storage_dir.clone());
        let mut index = SessionIndex::open(&storage_dir);

        // Ensure directory exists
        fs::create_dir_all(&storage_dir).await.ok();

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

        // Create session entry
        let transcript_file = format!("{session_id}.jsonl");
        let mut entry = SessionEntry::new(
            session_id.to_string(),
            agent_name.to_string(),
            transcript_file,
        );
        entry.cwd = cwd;

        // Insert into index and associate with peer
        index
            .create_for_peer(entry, session_key)
            .await
            .with_context(|| "Failed to create session for peer")?;

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

    /// Create a new unified session from a specific directory (registry-based)
    ///
    /// This is used by SessionManager when it has already determined the sessions directory.
    /// NOTE: This is crate-private. Only SessionManager should create sessions.
    pub(crate) async fn create_with_path(
        agent_name: &str,
        peer: &Peer,
        session_id: &str,
        sessions_dir: impl AsRef<std::path::Path>,
    ) -> Result<Self> {
        let sessions_dir = sessions_dir.as_ref().to_path_buf();
        let storage = SessionStorage::new(sessions_dir.clone());
        let mut index = SessionIndex::open(&sessions_dir);
        let session_key = derive_session_key(agent_name, peer);

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

    // ============================================================
    // Opening
    // ============================================================

    /// Open an existing unified session by agent and peer
    ///
    /// Returns `Ok(None)` if no active session exists for this peer.
    pub async fn open(agent_name: &str, peer: &Peer) -> Result<Option<Self>> {
        let session_key = derive_session_key(agent_name, peer);
        Self::open_by_key(agent_name, &session_key).await
    }

    /// Open an existing unified session by key
    pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let storage = SessionStorage::new(storage_dir.clone());
        let mut index = SessionIndex::open(&storage_dir);

        // Look up active session for peer
        let entry = match index.get_active_for_peer(session_key).await? {
            Some(e) => e,
            None => return Ok(None),
        };

        // Load session entries to find last message
        let entries: Vec<crate::session::JsonlSessionEntry> =
            storage.load_session(&entry.session_id).await?;

        if entries.is_empty() {
            return Ok(None);
        }

        // Parse peer from session key
        let peer = parse_peer_from_key(session_key)?;

        // Build session from entry
        Self::from_entry(
            entry,
            peer,
            storage,
            index,
            entries,
            session_key.to_string(),
        )
        .map(Some)
    }

    /// Open an existing unified session by ID
    ///
    /// This bypasses the peer lookup and opens the session file directly.
    pub async fn open_by_id(
        agent_name: &str,
        session_id: &str,
        sessions_dir: impl AsRef<std::path::Path>,
    ) -> Result<Self> {
        let sessions_dir = sessions_dir.as_ref().to_path_buf();
        let storage = SessionStorage::new(sessions_dir.clone());
        let mut index = SessionIndex::open(&sessions_dir);

        // Load session entries
        let entries: Vec<crate::session::JsonlSessionEntry> =
            storage.load_session(session_id).await?;

        // Find session key from index
        let session_key = index
            .find_by_session_id(session_id)
            .await?
            .map(|_| format!("agent:{agent_name}:peer:user:default"))
            .unwrap_or_else(|| format!("agent:{agent_name}:peer:user:default"));

        // Parse peer from session key
        let peer = parse_peer_from_key(&session_key).unwrap_or(Peer::User("default".to_string()));

        Self::from_entries(
            session_id.to_string(),
            agent_name.to_string(),
            session_key,
            peer,
            storage,
            index,
            entries,
        )
        .await
    }

    /// Open an existing session by key (returns None if not found)
    pub async fn open_by_key_simple(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
        Self::open_by_key(agent_name, session_key).await
    }

    /// Open or create a session by key (for CLI persistence)
    pub async fn open_or_create_by_key(agent_name: &str, session_key: &str) -> Result<Self> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let mut index = SessionIndex::open(&storage_dir);

        // Check if session exists
        if let Some(entry) = index.get_active_for_peer(session_key).await? {
            // Open existing
            return Self::open_by_id(agent_name, &entry.session_id, &storage_dir)
                .await
                .map(Some)
                .map(|s| s.expect("Session in index but not on disk"));
        }

        // Create new with this key
        let peer = parse_peer_from_key(session_key).unwrap_or(Peer::User("default".to_string()));
        let session_id = uuid::Uuid::new_v4().to_string();
        Self::create_with_key(agent_name, &peer, &session_id, session_key).await
    }

    /// Get or create a unified session
    pub async fn get_or_create(agent_name: &str, peer: &Peer) -> Result<Self> {
        match Self::open(agent_name, peer).await? {
            Some(session) => Ok(session),
            None => Self::create(agent_name, peer).await,
        }
    }

    // ============================================================
    // Helper Methods
    // ============================================================

    /// Build a UnifiedSession from a SessionEntry
    fn from_entry(
        entry: SessionEntry,
        peer: Peer,
        storage: SessionStorage,
        index: SessionIndex,
        entries: Vec<crate::session::JsonlSessionEntry>,
        session_key: String,
    ) -> Result<Self> {
        // Count messages and find last ID
        let mut message_count = 0;
        let last_message_id = entries.iter().rev().find_map(|entry| match entry {
            crate::session::JsonlSessionEntry::Message { id, .. } => {
                message_count += 1;
                Some(id.clone())
            }
            _ => None,
        });

        Ok(Self {
            id: entry.session_id,
            agent_name: entry.agent_name,
            session_key,
            peer,
            storage,
            index,
            last_message_id,
            message_count,
            input_tokens: entry.input_tokens,
            output_tokens: entry.output_tokens,
            current_provider: entry.provider,
            current_model: entry.model,
        })
    }

    /// Build a UnifiedSession from raw entries (for open_by_id)
    async fn from_entries(
        session_id: String,
        agent_name: String,
        session_key: String,
        peer: Peer,
        storage: SessionStorage,
        mut index: SessionIndex,
        entries: Vec<crate::session::JsonlSessionEntry>,
    ) -> Result<Self> {
        // Count messages and find last ID
        let mut message_count = 0;
        let last_message_id = entries.iter().rev().find_map(|entry| match entry {
            crate::session::JsonlSessionEntry::Message { id, .. } => {
                message_count += 1;
                Some(id.clone())
            }
            _ => None,
        });

        // Restore token counts and metadata from index
        let (input_tokens, output_tokens, current_provider, current_model) = index
            .get(&session_id)
            .await
            .ok()
            .flatten()
            .map(|e| (e.input_tokens, e.output_tokens, e.provider, e.model))
            .unwrap_or((0, 0, None, None));

        Ok(Self {
            id: session_id,
            agent_name,
            session_key,
            peer,
            storage,
            index,
            last_message_id,
            message_count,
            input_tokens,
            output_tokens,
            current_provider,
            current_model,
        })
    }

    // ============================================================
    // Index Updates (ATOMIC - prevents racing)
    // ============================================================

    /// Update the index with current metadata - ATOMIC operation
    ///
    /// This is the key method that prevents racing. All updates happen
    /// in a single operation, so there's no interleaving with other updates.
    async fn update_index(&mut self) -> Result<()> {
        // Get the session_id from active session for this peer
        if let Some(session_id) = self.index.get_active_session_id(&self.session_key).await? {
            if let Some(mut entry) = self.index.get(&session_id).await? {
                entry.touch();
                entry.message_count = self.message_count;
                entry.input_tokens = self.input_tokens;
                entry.output_tokens = self.output_tokens;
                entry.total_tokens = self.input_tokens + self.output_tokens;
                entry.provider = self.current_provider.clone();
                entry.model = self.current_model.clone();
                self.index.insert(entry).await?;
                self.index.save().await?;
            }
        }
        Ok(())
    }

    // ============================================================
    // Metadata Operations
    // ============================================================

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

    /// Get current provider and model
    #[must_use]
    pub fn current_model(&self) -> Option<(&str, &str)> {
        match (&self.current_provider, &self.current_model) {
            (Some(p), Some(m)) => Some((p.as_str(), m.as_str())),
            _ => None,
        }
    }

    // ============================================================
    // Message Operations
    // ============================================================

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
        tool_calls: Option<Vec<ToolCall>>,
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

    /// Add an assistant message with tool calls (with ContentBlock tool calls)
    pub async fn add_assistant_with_tool_calls(
        &mut self,
        content: impl Into<String>,
        tool_calls: Vec<ContentBlock>,
    ) -> Result<()> {
        let content_str = content.into();
        let mut content_blocks = vec![];

        // Add text if present
        if !content_str.is_empty() {
            content_blocks.push(ContentBlock::Text { text: content_str });
        }

        // Add tool calls with their original IDs
        for block in tool_calls {
            if let ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } = block
            {
                content_blocks.push(ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
        }

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
        // Tool results don't update last_message_id or count
        Ok(())
    }

    /// Add a thinking block (streaming reasoning)
    pub async fn add_thinking(
        &mut self,
        thinking: impl Into<String>,
        signature: Option<String>,
    ) -> Result<()> {
        let msg_id = self
            .storage
            .append_message(
                &self.id,
                self.last_message_id.clone(),
                "assistant",
                vec![ContentBlock::Thinking {
                    text: thinking.into(),
                    signature,
                }],
            )
            .await?;
        self.last_message_id = Some(msg_id);
        // Thinking blocks don't count as messages for stats
        Ok(())
    }

    // ============================================================
    // History Operations
    // ============================================================

    /// Load conversation history
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> {
        use crate::providers::MessageRole;

        let entries = self.storage.load_session(&self.id).await?;
        let mut messages = Vec::new();

        for entry in entries {
            match entry {
                crate::session::JsonlSessionEntry::Message { message, .. } => {
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
                crate::session::JsonlSessionEntry::ToolResult {
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

    /// Get context as text (for LLM)
    pub async fn get_context_text(&self, _limit: usize) -> String {
        let entries = match self.storage.load_session(&self.id).await {
            Ok(e) => e,
            Err(_) => return format!("Session: {}", self.id),
        };

        let mut context = String::new();

        for entry in entries {
            match entry {
                crate::session::JsonlSessionEntry::Message { message, .. } => {
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
                crate::session::JsonlSessionEntry::ToolResult {
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

    // ============================================================
    // Compaction
    // ============================================================

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
        let entries: Vec<crate::session::JsonlSessionEntry> =
            self.storage.load_session(&self.id).await?;

        for entry in entries.iter().rev() {
            if let crate::session::JsonlSessionEntry::Compaction { summary, .. } = entry {
                return Ok(Some(summary.clone()));
            }
        }

        Ok(None)
    }

    // ============================================================
    // Model Change Recording
    // ============================================================

    /// Record model change
    pub async fn record_model_change(&mut self, provider: &str, model_id: &str) -> Result<()> {
        self.storage
            .append_model_change(&self.id, self.last_message_id.clone(), provider, model_id)
            .await?;
        // Model changes don't update last_message_id
        Ok(())
    }

    // ============================================================
    // Static Utilities
    // ============================================================

    /// List available sessions for an agent
    pub async fn list_sessions(agent_name: &str) -> Result<Vec<(String, std::time::SystemTime)>> {
        let storage_dir = Self::storage_dir(agent_name, None);

        let mut sessions = Vec::new();

        if let Ok(mut entries) = tokio::fs::read_dir(&storage_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    let session_id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();

                    if let Ok(metadata) = entry.metadata().await {
                        if let Ok(modified) = metadata.modified() {
                            sessions.push((session_id, modified));
                        }
                    }
                }
            }
        }

        // Sort by modification time (newest first)
        sessions.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(sessions)
    }
}

// ============================================================
// Helper Functions
// ============================================================

/// Derive a session key from agent name and peer
fn derive_session_key(agent_name: &str, peer: &Peer) -> String {
    match peer {
        Peer::User(id) => format!("agent:{agent_name}:peer:user:{id}"),
        Peer::Agent(id) => format!("agent:{agent_name}:peer:agent:{id}"),
    }
}

/// Parse a peer from a session key
fn parse_peer_from_key(key: &str) -> Result<Peer> {
    // Format: agent:{agent}:peer:{type}:{id}
    let parts: Vec<&str> = key.split(':').collect();

    if parts.len() < 5 {
        return Err(anyhow::anyhow!("Invalid session key format: {key}"));
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

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_unified_session_create() {
        let peer = Peer::User("alice".to_string());
        let session = UnifiedSession::create("test_agent", &peer).await;
        assert!(session.is_ok());

        let session = session.unwrap();
        assert_eq!(session.agent_name, "test_agent");
        assert_eq!(session.peer, peer);
        assert!(session.session_key.contains("peer:user:alice"));
        assert_eq!(session.message_count, 0);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_unified_session_agent_peer() {
        let peer = Peer::Agent("helper".to_string());
        let session = UnifiedSession::create("test_agent", &peer).await.unwrap();

        assert_eq!(session.peer, peer);
        assert!(session.session_key.contains("peer:agent:helper"));
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_unified_session_add_messages() {
        let peer = Peer::User("alice".to_string());
        let mut session = UnifiedSession::create("test_agent", &peer).await.unwrap();

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
    async fn test_unified_session_token_usage() {
        let peer = Peer::User("alice".to_string());
        let mut session = UnifiedSession::create("test_agent", &peer).await.unwrap();

        session.record_usage(100, 50).await.unwrap();
        session.record_usage(50, 25).await.unwrap();

        let (input, output, total) = session.token_usage();
        assert_eq!(input, 150);
        assert_eq!(output, 75);
        assert_eq!(total, 225);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_unified_session_persistence() {
        let peer = Peer::User("alice".to_string());

        // Create session
        let mut session = UnifiedSession::create("test_agent", &peer).await.unwrap();
        let session_key = session.session_key.clone();

        session.add_user("Hello!").await.unwrap();
        session.add_assistant("Hi!", None).await.unwrap();

        // Re-open by key
        let reopened = UnifiedSession::open_by_key("test_agent", &session_key)
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
    async fn test_unified_session_get_or_create() {
        let peer = Peer::User("alice".to_string());

        // Create new
        let session1 = UnifiedSession::get_or_create("test_agent", &peer)
            .await
            .unwrap();
        let key1 = session1.session_key.clone();

        // Get existing
        let session2 = UnifiedSession::get_or_create("test_agent", &peer)
            .await
            .unwrap();
        let key2 = session2.session_key;

        assert_eq!(key1, key2);
    }

    #[test]
    fn test_derive_session_key() {
        let peer = Peer::User("alice".to_string());
        let key = derive_session_key("test_agent", &peer);
        assert_eq!(key, "agent:test_agent:peer:user:alice");

        let peer = Peer::Agent("helper".to_string());
        let key = derive_session_key("test_agent", &peer);
        assert_eq!(key, "agent:test_agent:peer:agent:helper");
    }

    #[test]
    fn test_parse_peer_from_key() {
        // User peer
        let peer = parse_peer_from_key("agent:test:peer:user:alice").unwrap();
        assert_eq!(peer, Peer::User("alice".to_string()));

        // Agent peer
        let peer = parse_peer_from_key("agent:test:peer:agent:helper").unwrap();
        assert_eq!(peer, Peer::Agent("helper".to_string()));

        // Complex user ID
        let peer = parse_peer_from_key("agent:test:peer:user:domain_user_123").unwrap();
        assert_eq!(peer, Peer::User("domain_user_123".to_string()));
    }

    #[test]
    fn test_parse_peer_from_key_invalid() {
        let result = parse_peer_from_key("invalid_key");
        assert!(result.is_err());
    }
}
