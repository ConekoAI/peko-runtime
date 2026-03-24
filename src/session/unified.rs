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

use crate::common::paths::PathResolver;
use crate::engine::ToolCall;
use crate::providers::ChatMessage;
use crate::session::events::{
    generate_event_id, generate_message_id, AssistantMessageEvent, EventEnvelope, MessageEvent,
    MessageSource, SessionEvent, SystemMessageEvent, ThinkingEvent, TokenUsage as EventTokenUsage,
    ToolResultEvent, UserMessageEvent,
};
use crate::providers::TokenUsage;
use crate::session::index::SessionEntry;
use crate::session::jsonl::SessionStorage;
use crate::session::types::Peer;
use crate::types::ContentBlock;
use anyhow::{Context, Result};
use chrono::Utc;
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
/// let session = UnifiedSession::create("my_agent", &peer, Some("default")).await?;
///
/// // Add messages
/// session.add_user("Hello!").await?;
/// session.add_assistant("Hi there!", None, None).await?;
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
    /// Uses team-based structure: `{data_dir}/sessions/{team}/{agent}/`
    ///
    /// DEPRECATED: Use `PathResolver::agent_sessions_dir()` instead.
    /// This method is kept for backward compatibility.
    ///
    /// # Arguments
    /// * `base_dir` - **Ignored**, kept for backward compatibility
    /// * `agent_name` - The agent name
    /// * `team` - Optional team name (defaults to "default")
    #[must_use]
    #[deprecated(
        since = "0.9.0",
        note = "Use PathResolver::agent_sessions_dir() instead"
    )]
    pub fn storage_dir(
        _base_dir: Option<&std::path::Path>,
        agent_name: &str,
        team: Option<&str>,
    ) -> PathBuf {
        // Delegate to PathResolver for consistent path resolution
        let resolver = crate::common::paths::PathResolver::new();
        resolver.agent_sessions_dir(agent_name, team)
    }

    // ============================================================
    // Creation
    // ============================================================

    /// Create a new unified session for an agent and peer
    ///
    /// # Arguments
    /// * `agent_name` - The agent name
    /// * `peer` - The peer this session belongs to
    /// * `team` - Optional team name (defaults to "default")
    ///
    /// NOTE: This is crate-private. Only SessionManager should create sessions.
    pub(crate) async fn create(agent_name: &str, peer: &Peer, team: Option<&str>) -> Result<Self> {
        let session_key = crate::session::key::derive_base_session_key(agent_name, peer);
        let session_id = uuid::Uuid::new_v4().to_string();

        Self::create_with_key(agent_name, peer, &session_id, &session_key, team).await
    }

    /// Create a new unified session with explicit PathResolver
    ///
    /// This is the PREFERRED method as it ensures consistent path resolution.
    ///
    /// # Arguments
    /// * `resolver` - PathResolver for consistent path resolution
    /// * `agent_name` - The agent name
    /// * `peer` - The peer this session belongs to
    /// * `team` - Optional team name
    ///
    /// NOTE: This is crate-private. Only SessionManager should create sessions.
    pub(crate) async fn create_with_resolver(
        resolver: &crate::common::paths::PathResolver,
        agent_name: &str,
        peer: &Peer,
        team: Option<&str>,
    ) -> Result<Self> {
        let session_key = crate::session::key::derive_base_session_key(agent_name, peer);
        let session_id = uuid::Uuid::new_v4().to_string();
        let sessions_dir = resolver.agent_sessions_dir(agent_name, team);

        Self::create_with_path(agent_name, peer, &session_id, sessions_dir).await
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
        team: Option<&str>,
    ) -> Result<Self> {
        let storage_dir = Self::storage_dir(None, agent_name, team);
        let storage = SessionStorage::new(storage_dir.clone());

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

        // Note: Index entry is created by SessionManager/MetadataController
        // UnifiedSession only manages the JSONL file

        Ok(Self {
            id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            session_key: session_key.to_string(),
            peer: peer.clone(),
            storage,
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
        let session_key = crate::session::key::derive_base_session_key(agent_name, peer);

        // Ensure directory exists
        fs::create_dir_all(&sessions_dir).await?;

        // Create session file
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());

        storage.create_session(session_id, cwd.clone()).await?;

        // Note: Index entry is created by SessionManager/MetadataController
        // UnifiedSession only manages the JSONL file

        Ok(Self {
            id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            session_key,
            peer: peer.clone(),
            storage,
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
        let session_key = crate::session::key::derive_base_session_key(agent_name, peer);
        Self::open_by_key(agent_name, &session_key).await
    }

    /// Open an existing unified session by key
    /// Note: Session ID lookup should be done by SessionManager using MetadataController
    pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
        // This method requires SessionManager to look up the session ID from the index
        // UnifiedSession no longer accesses the index directly
        let _ = (agent_name, session_key);
        Ok(None)
    }

    /// Open an existing unified session by ID
    ///
    /// This bypasses the peer lookup and opens the session file directly.
    /// JSONL is the source of truth for message count and content.
    ///
    /// # Arguments
    /// * `agent_name` - The agent name
    /// * `session_id` - The session ID
    /// * `sessions_dir` - The sessions directory
    /// * `peer` - Optional peer info. If provided, restores session identity from this peer.
    ///            If None, defaults to Peer::User("default")
    pub async fn open_by_id(
        agent_name: &str,
        session_id: &str,
        sessions_dir: impl AsRef<std::path::Path>,
        peer: Option<&Peer>,
    ) -> Result<Self> {
        let sessions_dir = sessions_dir.as_ref().to_path_buf();
        let storage = SessionStorage::new(sessions_dir.clone());

        // Load session entries
        let entries: Vec<crate::session::JsonlSessionEntry> =
            storage.load_session(session_id).await?;

        // Use provided peer or default
        let peer = peer
            .cloned()
            .unwrap_or_else(|| Peer::User("default".to_string()));
        let session_key = crate::session::key::derive_base_session_key(agent_name, &peer);

        Self::from_entries(
            session_id.to_string(),
            agent_name.to_string(),
            session_key,
            peer,
            storage,
            entries,
        )
        .await
    }

    /// Open an existing session by key (returns None if not found)
    /// Note: Use SessionManager for proper session lookup
    pub async fn open_by_key_simple(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
        Self::open_by_key(agent_name, session_key).await
    }

    /// Open or create a session by key (for CLI persistence)
    /// Note: SessionManager should be used for production code
    pub async fn open_or_create_by_key(
        agent_name: &str,
        session_key: &str,
        team: Option<&str>,
    ) -> Result<Self> {
        // Use PathResolver for consistent path resolution
        let resolver = crate::common::paths::PathResolver::new();
        let storage_dir = resolver.agent_sessions_dir(agent_name, team);

        // Try to find existing session file
        // Note: This is a simplified version - SessionManager does proper lookup
        if let Ok(mut entries) = tokio::fs::read_dir(&storage_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                        let peer = parse_peer_from_key(session_key)
                            .unwrap_or(Peer::User("default".to_string()));
                        return Self::open_by_id(agent_name, name, &storage_dir, Some(&peer)).await;
                    }
                }
            }
        }

        // Create new with this key
        let peer = parse_peer_from_key(session_key).unwrap_or(Peer::User("default".to_string()));
        let session_id = uuid::Uuid::new_v4().to_string();
        Self::create_with_path(agent_name, &peer, &session_id, storage_dir).await
    }

    /// Get or create a unified session
    pub async fn get_or_create(agent_name: &str, peer: &Peer, team: Option<&str>) -> Result<Self> {
        match Self::open(agent_name, peer).await? {
            Some(session) => Ok(session),
            None => Self::create(agent_name, peer, team).await,
        }
    }

    // ============================================================
    // Helper Methods
    // ============================================================

    /// Build a UnifiedSession from a SessionEntry
    /// Note: Token counts and model info are loaded from JSONL or defaults
    async fn from_entry(
        entry: SessionEntry,
        peer: Peer,
        storage: SessionStorage,
        entries: Vec<crate::session::JsonlSessionEntry>,
        session_key: String,
    ) -> Result<Self> {
        let session_id = entry.session_id.clone();

        // Count messages and find last ID (JSONL is source of truth)
        let mut message_count = 0;
        let mut last_message_id = None;

        for e in entries.iter().rev() {
            if let crate::session::JsonlSessionEntry::Message { id, .. } = e {
                message_count += 1;
                if last_message_id.is_none() {
                    last_message_id = Some(id.clone());
                }
            }
        }

        // Note: Token counts and provider/model should be loaded from JSONL ModelChange entries
        // or set by caller (MetadataController) after loading
        Ok(Self {
            id: session_id,
            agent_name: entry.agent_name,
            session_key,
            peer,
            storage,
            last_message_id,
            message_count,
            input_tokens: entry.input_tokens,
            output_tokens: entry.output_tokens,
            current_provider: entry.provider,
            current_model: entry.model,
        })
    }

    /// Build a UnifiedSession from raw entries (for open_by_id)
    /// JSONL is the source of truth for message count.
    async fn from_entries(
        session_id: String,
        agent_name: String,
        session_key: String,
        peer: Peer,
        storage: SessionStorage,
        entries: Vec<crate::session::JsonlSessionEntry>,
    ) -> Result<Self> {
        // Count messages and find last ID (JSONL is source of truth)
        let mut message_count = 0;
        let mut last_message_id = None;

        for entry in entries.iter().rev() {
            if let crate::session::JsonlSessionEntry::Message { id, .. } = entry {
                message_count += 1;
                if last_message_id.is_none() {
                    last_message_id = Some(id.clone());
                }
            }
        }

        // Note: Token counts and provider/model info are not stored in JSONL messages
        // They should be tracked by MetadataController separately
        // or we could add ModelChange entry tracking here in the future

        Ok(Self {
            id: session_id,
            agent_name,
            session_key,
            peer,
            storage,
            last_message_id,
            message_count,
            input_tokens: 0,
            output_tokens: 0,
            current_provider: None,
            current_model: None,
        })
    }

    // ============================================================
    // Metadata Operations
    // ============================================================

    /// Record token usage (in-memory only, persists to index via MetadataController)
    pub fn record_usage(&mut self, input: usize, output: usize) {
        self.input_tokens += input;
        self.output_tokens += output;
    }

    /// Set the current model (in-memory only, persists to index via MetadataController)
    pub fn set_model(&mut self, provider: &str, model: &str) {
        self.current_provider = Some(provider.to_string());
        self.current_model = Some(model.to_string());
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
    ///
    /// Writes as Event Format (system.message) for consistency with the Pekobot
    /// session specification.
    pub async fn add_system(&mut self, content: impl Into<String>) -> Result<()> {
        let content_str = content.into();
        let msg_id = generate_message_id();

        let event = SessionEvent::SystemMessage(SystemMessageEvent {
            envelope: EventEnvelope {
                id: generate_event_id(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            content: content_str,
        });

        self.storage.append_event(&self.id, &event).await?;
        self.last_message_id = Some(msg_id);
        self.message_count += 1;
        Ok(())
    }

    /// Add a user message
    ///
    /// Writes as Event Format (user.message) for consistency with the Pekobot
    /// session specification (DATA_MODEL.md §5.3).
    pub async fn add_user(&mut self, content: impl Into<String>) -> Result<()> {
        let content_str = content.into();
        let msg_id = generate_message_id();

        let event = SessionEvent::UserMessage(UserMessageEvent {
            envelope: EventEnvelope {
                id: generate_event_id(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            message_id: msg_id.clone(),
            content: content_str,
            source: MessageSource::User,
        });

        self.storage.append_event(&self.id, &event).await?;
        self.last_message_id = Some(msg_id);
        self.message_count += 1;
        Ok(())
    }

    /// Add an assistant message with optional tool calls
    ///
    /// Writes as Event Format (assistant.message) for consistency with the Pekobot
    /// session specification (DATA_MODEL.md §5.3).
    ///
    /// Note: Tool calls are currently stored in the message content as they are
    /// part of the assistant's response. Dedicated tool.call events may be added
    /// in the future for more granular tracking.
    pub async fn add_assistant(
        &mut self,
        content: impl Into<String>,
        tool_calls: Option<Vec<ToolCall>>,
        usage: Option<TokenUsage>,
    ) -> Result<()> {
        let content_str = content.into();

        // Update token usage if provided
        if let Some(u) = usage {
            self.record_usage(u.input as usize, u.output as usize);
        }

        // For tool calls, we include them in the content for now
        // The Event Format assistant.message has a simple text content field
        // TODO: Add separate tool.call events for granular tool tracking
        let final_content = if let Some(calls) = tool_calls {
            let mut full_content = content_str;
            for call in calls.iter() {
                let tool_call_str = format!(
                    "\n[ToolCall: {}({})]",
                    call.name,
                    serde_json::to_string(&call.parameters).unwrap_or_default()
                );
                full_content.push_str(&tool_call_str);
            }
            full_content
        } else {
            content_str
        };

        let msg_id = generate_message_id();

        let event = SessionEvent::AssistantMessage(AssistantMessageEvent {
            envelope: EventEnvelope {
                id: generate_event_id(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            message_id: msg_id.clone(),
            content: final_content,
            usage: EventTokenUsage {
                input_tokens: self.input_tokens as u32,
                output_tokens: self.output_tokens as u32,
                total_tokens: (self.input_tokens + self.output_tokens) as u32,
            },
        });

        self.storage.append_event(&self.id, &event).await?;
        self.last_message_id = Some(msg_id);
        self.message_count += 1;
        Ok(())
    }

    /// Add an assistant message with tool calls (with ContentBlock tool calls)
    ///
    /// Writes as Event Format (assistant.message) for consistency with the Pekobot
    /// session specification (DATA_MODEL.md §5.3).
    pub async fn add_assistant_with_tool_calls(
        &mut self,
        content: impl Into<String>,
        tool_calls: Vec<ContentBlock>,
        usage: Option<TokenUsage>,
    ) -> Result<()> {
        let content_str = content.into();
        let mut final_content = content_str;

        // Update token usage if provided
        if let Some(u) = usage {
            self.record_usage(u.input as usize, u.output as usize);
        }

        // Add tool calls as text annotations
        for block in tool_calls {
            if let ContentBlock::ToolCall {
                name, arguments, ..
            } = block
            {
                let tool_call_str = format!(
                    "\n[ToolCall: {}({})]",
                    name,
                    serde_json::to_string(&arguments).unwrap_or_default()
                );
                final_content.push_str(&tool_call_str);
            }
        }

        let msg_id = generate_message_id();

        let event = SessionEvent::AssistantMessage(AssistantMessageEvent {
            envelope: EventEnvelope {
                id: generate_event_id(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            message_id: msg_id.clone(),
            content: final_content,
            usage: EventTokenUsage {
                input_tokens: self.input_tokens as u32,
                output_tokens: self.output_tokens as u32,
                total_tokens: (self.input_tokens + self.output_tokens) as u32,
            },
        });

        self.storage.append_event(&self.id, &event).await?;
        self.last_message_id = Some(msg_id);
        self.message_count += 1;
        Ok(())
    }

    /// Add a tool result
    ///
    /// Writes as Event Format (tool.result) for consistency with the Pekobot
    /// session specification (DATA_MODEL.md §5.3).
    pub async fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        _tool_name: impl Into<String>,
        result: impl Into<String>,
    ) -> Result<()> {
        let tool_call_id_str = tool_call_id.into();
        let result_str = result.into();

        let event = SessionEvent::ToolResult(ToolResultEvent {
            envelope: EventEnvelope {
                id: generate_event_id(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            tool_call_id: tool_call_id_str,
            output: Some(result_str),
            error: None,
            duration_ms: 0, // TODO: Track actual duration
        });

        self.storage.append_event(&self.id, &event).await?;
        // Tool results don't update last_message_id or count
        Ok(())
    }

    /// Add a thinking block (streaming reasoning)
    ///
    /// Writes as Event Format (thinking) for consistency with the Pekobot
    /// session specification (DATA_MODEL.md §5.3).
    pub async fn add_thinking(
        &mut self,
        thinking: impl Into<String>,
        _signature: Option<String>,
    ) -> Result<()> {
        let content = thinking.into();

        let event = SessionEvent::Thinking(ThinkingEvent {
            envelope: EventEnvelope {
                id: generate_event_id(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            content,
        });

        self.storage.append_event(&self.id, &event).await?;
        // Thinking blocks don't count as messages for stats
        Ok(())
    }

    // ============================================================
    // History Operations
    // ============================================================

    /// Load conversation history
    ///
    /// Uses normalized loading to support both Legacy V3 and Event Format sessions.
    /// This ensures backward compatibility during the format transition.
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> {
        use crate::providers::MessageRole;
        use crate::session::NormalizedEntry;

        let entries = self.storage.load_normalized(&self.id).await?;
        let mut messages = Vec::new();

        for entry in entries {
            match entry {
                NormalizedEntry::UserMessage { content, .. } => {
                    messages.push(ChatMessage {
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text { text: content }],
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                NormalizedEntry::AssistantMessage { content, .. } => {
                    messages.push(ChatMessage {
                        role: MessageRole::Assistant,
                        content: vec![ContentBlock::Text { text: content }],
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                NormalizedEntry::SystemMessage { content, .. } => {
                    messages.push(ChatMessage {
                        role: MessageRole::System,
                        content: vec![ContentBlock::Text { text: content }],
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                NormalizedEntry::ToolResult {
                    content,
                    tool_call_id,
                    ..
                } => {
                    messages.push(ChatMessage {
                        role: MessageRole::Tool,
                        content: vec![ContentBlock::Text { text: content }],
                        tool_calls: None,
                        tool_call_id: Some(tool_call_id),
                    });
                }
                // Session headers and other entries don't contribute to chat history
                _ => {}
            }
        }

        Ok(messages)
    }

    /// Get context as text (for LLM)
    ///
    /// Uses normalized loading to support both Legacy V3 and Event Format sessions.
    pub async fn get_context_text(&self, _limit: usize) -> String {
        use crate::session::NormalizedEntry;

        let entries = match self.storage.load_normalized(&self.id).await {
            Ok(e) => e,
            Err(_) => return format!("Session: {}", self.id),
        };

        let mut context = String::new();

        for entry in entries {
            match entry {
                NormalizedEntry::UserMessage { content, .. } => {
                    if !content.is_empty() {
                        context.push_str(&format!("user: {content}\n\n"));
                    }
                }
                NormalizedEntry::AssistantMessage { content, .. } => {
                    if !content.is_empty() {
                        context.push_str(&format!("assistant: {content}\n\n"));
                    }
                }
                NormalizedEntry::SystemMessage { content, .. } => {
                    if !content.is_empty() {
                        context.push_str(&format!("system: {content}\n\n"));
                    }
                }
                NormalizedEntry::ToolResult {
                    content, tool_name, ..
                } => {
                    context.push_str(&format!("tool: [{tool_name} result: {content}]\n\n"));
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
    ///
    /// Uses normalized loading to support both Legacy V3 and Event Format sessions.
    pub async fn load_previous_compaction_summary(&self) -> Result<Option<String>> {
        use crate::session::NormalizedEntry;

        let entries = self.storage.load_normalized(&self.id).await?;

        for entry in entries.iter().rev() {
            if let NormalizedEntry::Compaction { summary, .. } = entry {
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
    pub async fn list_sessions(
        agent_name: &str,
        team: Option<&str>,
    ) -> Result<Vec<(String, std::time::SystemTime)>> {
        // Use PathResolver for consistent path resolution
        let resolver = crate::common::paths::PathResolver::new();
        let storage_dir = resolver.agent_sessions_dir(agent_name, team);

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
        let session = UnifiedSession::create("test_agent", &peer, None).await;
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
        let session = UnifiedSession::create("test_agent", &peer, None)
            .await
            .unwrap();

        assert_eq!(session.peer, peer);
        assert!(session.session_key.contains("peer:agent:helper"));
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_unified_session_add_messages() {
        let peer = Peer::User("alice".to_string());
        let mut session = UnifiedSession::create("test_agent", &peer, None)
            .await
            .unwrap();

        session
            .add_system("You are a helpful assistant")
            .await
            .unwrap();
        session.add_user("Hello!").await.unwrap();
        session.add_assistant("Hi there!", None, None).await.unwrap();

        assert_eq!(session.message_count, 3);

        let history = session.load_history().await.unwrap();
        assert_eq!(history.len(), 3);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_unified_session_token_usage() {
        let peer = Peer::User("alice".to_string());
        let mut session = UnifiedSession::create("test_agent", &peer, None)
            .await
            .unwrap();

        session.record_usage(100, 50);
        session.record_usage(50, 25);

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
        let mut session = UnifiedSession::create("test_agent", &peer, None)
            .await
            .unwrap();
        let session_key = session.session_key.clone();

        session.add_user("Hello!").await.unwrap();
        session.add_assistant("Hi!", None, None).await.unwrap();

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
        let session1 = UnifiedSession::get_or_create("test_agent", &peer, None)
            .await
            .unwrap();
        let key1 = session1.session_key.clone();

        // Get existing
        let session2 = UnifiedSession::get_or_create("test_agent", &peer, None)
            .await
            .unwrap();
        let key2 = session2.session_key;

        assert_eq!(key1, key2);
    }

    #[test]
    fn test_derive_session_key() {
        let peer = Peer::User("alice".to_string());
        let key = crate::session::key::derive_base_session_key("test_agent", &peer);
        assert_eq!(key, "agent:test_agent:peer:user:alice");

        let peer = Peer::Agent("helper".to_string());
        let key = crate::session::key::derive_base_session_key("test_agent", &peer);
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
