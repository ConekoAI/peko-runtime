//! Simple session persistence with OpenClaw-compatible JSONL format
//!
//! Wraps the new JSONL session storage for mandatory persistence.
//! Now with session index integration for fast lookups.

use crate::engine::ToolCall;
use crate::providers::ChatMessage;
use crate::session::index::{SessionEntry, SessionIndex};
use crate::session::jsonl::SessionStorage;
use crate::types::ContentBlock;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Simple session that auto-persists in OpenClaw-compatible format
pub struct SimpleSession {
    /// Session ID
    pub id: String,
    /// Agent name
    pub agent_name: String,
    /// Optional session key (for indexed lookup)
    pub session_key: Option<String>,
    /// Storage
    storage: SessionStorage,
    /// Session index for metadata tracking
    index: SessionIndex,
    /// Last message ID (for parent chaining)
    last_message_id: Option<String>,
    /// Message count for index updates
    message_count: usize,
    /// Input tokens for index updates
    input_tokens: usize,
    /// Output tokens for index updates
    output_tokens: usize,
    /// Current provider (e.g., "anthropic", "openai")
    current_provider: Option<String>,
    /// Current model (e.g., "claude-3-5-sonnet")
    current_model: Option<String>,
}

impl SimpleSession {
    /// Get the storage directory for an agent
    /// Uses team-based structure: ~/.pekobot/teams/{team}/agents/{agent}/sessions/
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

    /// Create a new session for an agent
    pub async fn create(agent_name: &str) -> Result<Self> {
        let session_id = format!("{}_{}", agent_name, chrono::Utc::now().timestamp_millis());
        Self::create_with_id(agent_name, &session_id).await
    }

    /// Create a new session for an agent with a specific session ID
    pub async fn create_with_id(agent_name: &str, session_id: &str) -> Result<Self> {
        Self::create_with_key(agent_name, session_id, None).await
    }

    /// Create a new session with an optional session key for indexing
    pub async fn create_with_key(
        agent_name: &str,
        session_id: &str,
        session_key: Option<String>,
    ) -> Result<Self> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let storage = SessionStorage::new(storage_dir.clone());
        let mut index = SessionIndex::open(&storage_dir);

        // Create session entry
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
        let mut entry = SessionEntry::new(
            session_id.to_string(),
            agent_name.to_string(),
            transcript_file,
        );
        entry.cwd = cwd;

        // Insert into index using create_for_peer
        let peer_key = session_key
            .clone()
            .unwrap_or_else(|| format!("agent:{agent_name}:session:{session_id}"));
        index
            .create_for_peer(entry, &peer_key)
            .await
            .with_context(|| "Failed to insert into index")?;

        Ok(Self {
            id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            session_key,
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

    /// Open an existing session by ID
    pub async fn open(agent_name: &str, session_id: &str) -> Result<Option<Self>> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let storage = SessionStorage::new(storage_dir.clone());
        let mut index = SessionIndex::open(&storage_dir);

        // Load existing entries to find the last message ID
        let entries: Vec<crate::session::jsonl::SessionEntry> =
            storage.load_session(session_id).await?;

        if entries.is_empty() {
            return Ok(None);
        }

        // Count messages and find last ID
        let mut message_count = 0;
        let last_message_id = entries.iter().rev().find_map(|entry| match entry {
            crate::session::jsonl::SessionEntry::Message { id, .. } => {
                message_count += 1;
                Some(id.clone())
            }
            _ => None,
        });

        // Find session key and token counts from index if available
        let index_entry = index.find_by_session_id(session_id).await?;
        let session_key = None; // session_key is now in peer mapping, not in entry
        let input_tokens = index_entry.as_ref().map(|e| e.input_tokens).unwrap_or(0);
        let output_tokens = index_entry.as_ref().map(|e| e.output_tokens).unwrap_or(0);
        let current_provider = index_entry.as_ref().and_then(|e| e.provider.clone());
        let current_model = index_entry.as_ref().and_then(|e| e.model.clone());

        Ok(Some(Self {
            id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            session_key,
            storage,
            index,
            last_message_id,
            message_count,
            input_tokens,
            output_tokens,
            current_provider,
            current_model,
        }))
    }

    /// Open or create a session by key (for CLI persistence)
    pub async fn open_or_create_by_key(agent_name: &str, session_key: &str) -> Result<Self> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let mut index = SessionIndex::open(&storage_dir);

        // Check if session exists
        if let Some(entry) = index.get_active_for_peer(session_key).await? {
            // Open existing
            return Self::open(agent_name, &entry.session_id)
                .await
                .map(|s| s.expect("Session in index but not on disk"));
        }

        // Create new with this key
        let session_id = format!("{}_{}", agent_name, chrono::Utc::now().timestamp_millis());
        Self::create_with_key(agent_name, &session_id, Some(session_key.to_string())).await
    }

    /// Open an existing session by key (returns None if not found)
    pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
        let storage_dir = Self::storage_dir(agent_name, None);
        let mut index = SessionIndex::open(&storage_dir);

        // Check if session exists
        if let Some(entry) = index.get_active_for_peer(session_key).await? {
            // Open existing
            Self::open(agent_name, &entry.session_id).await
        } else {
            Ok(None)
        }
    }

    /// Update the index with current session metadata
    async fn update_index(&mut self) -> Result<()> {
        if let Some(mut entry) = self.index.get(&self.id).await? {
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
        Ok(())
    }

    /// Record token usage from an LLM response
    pub async fn record_usage(&mut self, input_tokens: usize, output_tokens: usize) -> Result<()> {
        self.input_tokens += input_tokens;
        self.output_tokens += output_tokens;
        self.update_index().await?;
        Ok(())
    }

    /// Set the current provider and model
    pub async fn set_model(&mut self, provider: &str, model: &str) -> Result<()> {
        self.current_provider = Some(provider.to_string());
        self.current_model = Some(model.to_string());
        self.update_index().await?;
        Ok(())
    }

    /// Get current token usage
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

    /// Load conversation history for resumption
    /// Returns `ChatMessages` that can be fed to the LLM
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> {
        use crate::providers::MessageRole;

        let entries = self.storage.load_session(&self.id).await?;
        let mut messages = Vec::new();

        for entry in entries {
            match entry {
                crate::session::jsonl::SessionEntry::Message { message, .. } => {
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
                crate::session::jsonl::SessionEntry::ToolResult {
                    tool_call_id,
                    content,
                    is_error: _,
                    ..
                } => {
                    // Tool results become tool role messages
                    messages.push(ChatMessage {
                        role: MessageRole::Tool,
                        content,
                        tool_calls: None,
                        tool_call_id: Some(tool_call_id),
                    });
                }
                _ => {}
            }
        }

        Ok(messages)
    }

    /// List available sessions for an agent
    pub async fn list_sessions(agent_name: &str) -> Result<Vec<(String, std::time::SystemTime)>> {
        let storage_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("agents")
            .join(agent_name)
            .join("sessions");

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

    /// Add system message
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

    /// Add user message
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

    /// Add assistant message with tool calls
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

    /// Add assistant message (legacy API for text-only)
    pub async fn add_assistant(
        &mut self,
        content: impl Into<String>,
        tool_calls: Option<Vec<ToolCall>>,
    ) -> Result<()> {
        let content_str = content.into();

        let content_blocks = if let Some(calls) = tool_calls {
            // Build content blocks with tool calls
            let mut blocks = vec![];

            // Add text if present
            if !content_str.is_empty() {
                blocks.push(ContentBlock::Text { text: content_str });
            }

            // Add tool calls
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

    /// Add tool result
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
        // Tool results don't update last_message_id (they're not messages)
        Ok(())
    }

    /// Add thinking block (streaming reasoning)
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
        Ok(())
    }

    /// Get context as text (for LLM)
    pub async fn get_context_text(&self, _limit: usize) -> String {
        // Load session entries and format as conversation context
        let entries = match self.storage.load_session(&self.id).await {
            Ok(e) => e,
            Err(_) => return format!("Session: {}", self.id),
        };

        let mut context = String::new();

        for entry in entries {
            match entry {
                crate::session::jsonl::SessionEntry::Message { message, .. } => {
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
                crate::session::jsonl::SessionEntry::ToolResult {
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

    /// Record model change
    pub async fn record_model_change(&mut self, provider: &str, model_id: &str) -> Result<()> {
        let entry_id = self
            .storage
            .append_model_change(&self.id, self.last_message_id.clone(), provider, model_id)
            .await?;
        // Model changes don't update last_message_id
        let _ = entry_id;
        Ok(())
    }

    /// Record compaction event
    pub async fn record_compaction(
        &mut self,
        summary: &str,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
    ) -> Result<()> {
        let entry_id = self
            .storage
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
        // Compaction entries don't update last_message_id
        let _ = entry_id;
        Ok(())
    }

    /// Load the most recent compaction summary from session
    pub async fn load_previous_compaction_summary(&self) -> Result<Option<String>> {
        let entries: Vec<crate::session::jsonl::SessionEntry> =
            self.storage.load_session(&self.id).await?;

        // Find the most recent compaction entry
        for entry in entries.iter().rev() {
            if let crate::session::jsonl::SessionEntry::Compaction { summary, .. } = entry {
                return Ok(Some(summary.clone()));
            }
        }

        Ok(None)
    }
}
