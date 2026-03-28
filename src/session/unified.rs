//! Unified session implementation
//!
//! This module provides a single, authoritative session implementation that
//! manages conversation persistence via JSONL files.
//!
//! ## Design Principles
//!
//! 1. **Single Source of Truth**: One implementation manages all session data
//! 2. **Atomic Updates**: All index updates happen together in one operation
//! 3. **Clear Ownership**: SessionManager is the SOLE authority for session lifecycle
//! 4. **Backward Compatible**: Works with existing session files
//!
//! ## Important: SessionManager is the ONLY Way
//!
//! As of Phase 3 refactor, **all session creation and opening MUST go through
//! `SessionManager`**. UnifiedSession is now an internal implementation detail.
//! External code should use `SessionHandle` obtained from SessionManager.

use crate::engine::ToolCall;
use crate::providers::ChatMessage;
use crate::providers::TokenUsage as ProviderTokenUsage;
use crate::session::events::{
    generate_event_id, generate_message_id, EventEnvelope, SessionEvent, ToolCallBlock,
};
use crate::session::message::SessionMessage;
use crate::session::index::SessionEntry;
use crate::session::jsonl::SessionStorage;
use crate::session::types::Peer;
use crate::types::ContentBlock;
use anyhow::Result;
use chrono::Utc;
// ====================================================================================
// Message Conversion Functions (Phase 4a: Extracted from UnifiedSession)
// ====================================================================================
use crate::session::NormalizedEntry;

/// Convert a SessionEvent to a ChatMessage
///
/// This function handles the conversion from internal event format to
/// provider-agnostic ChatMessage format.
///
/// Uses the unified `as_message()` method to support both the new MessageV2
/// format and all legacy formats seamlessly.
pub(crate) fn event_to_chat_message(event: &SessionEvent) -> Option<ChatMessage> {
    // Use unified conversion for all message types (handles MessageV2 and legacy)
    if let Some(msg) = event.as_message() {
        return Some(msg.to_chat_message());
    }

    // Non-message events return None
    None
}

/// Convert a slice of NormalizedEntry to context text
///
/// This function extracts text content from normalized entries for LLM context.
pub(crate) fn entries_to_context_text(entries: &[NormalizedEntry]) -> String {
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
            // Other entry types don't contribute to context text
            _ => {}
        }
    }

    context
}

/// Unified session - internal implementation for conversation persistence
///
/// **IMPORTANT**: This is an internal implementation detail. Do not use directly.
/// All session operations should go through `SessionManager` which provides
/// `SessionHandle` for external use.
///
/// UnifiedSession manages the JSONL file storage for conversation history.
/// It is created and opened only by SessionManager.
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
    /// Current context window size (total_tokens from last assistant message)
    pub context_window: usize,
    /// Cumulative input tokens across all assistant messages
    pub total_input_tokens: usize,
    /// Cumulative output tokens across all assistant messages
    pub total_output_tokens: usize,
    /// Current provider
    pub current_provider: Option<String>,
    /// Current model
    pub current_model: Option<String>,
}

impl UnifiedSession {
    // ============================================================
    // Creation
    // ============================================================

    /// Create a UnifiedSession from components (used by SessionManager after JSONL creation)
    ///
    /// This is a low-level constructor. Prefer using `open_by_id` for opening existing sessions.
    pub(crate) fn from_components(
        session_id: String,
        agent_name: String,
        session_key: String,
        peer: Peer,
        storage: SessionStorage,
    ) -> Self {
        Self {
            id: session_id,
            agent_name,
            session_key,
            peer,
            storage,
            last_message_id: None,
            message_count: 0,
            context_window: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            current_provider: None,
            current_model: None,
        }
    }

    // ============================================================
    // Opening
    // ============================================================

    /// Open an existing unified session by ID
    ///
    /// This is the ONLY way to open a UnifiedSession. It requires the session ID
    /// which must be obtained from MetadataController via SessionManager.
    ///
    /// NOTE: All session opening must go through SessionManager::open_session().
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

        // Load normalized entries (supports both new SessionEvent and legacy SessionEntry formats)
        let entries = storage.load_normalized(session_id).await?;

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

    // ============================================================
    // Helper Methods
    // ============================================================

    /// Build a UnifiedSession from normalized entries (supports both new and legacy formats)
    /// JSONL is the source of truth for message count.
    async fn from_entries(
        session_id: String,
        agent_name: String,
        session_key: String,
        peer: Peer,
        storage: SessionStorage,
        entries: Vec<NormalizedEntry>,
    ) -> Result<Self> {
        // Count messages and find last ID (JSONL is source of truth)
        let mut message_count = 0;
        let mut last_message_id = None;

        for entry in entries.iter().rev() {
            match entry {
                NormalizedEntry::UserMessage { id, .. }
                | NormalizedEntry::AssistantMessage { id, .. } => {
                    message_count += 1;
                    if last_message_id.is_none() {
                        last_message_id = Some(id.clone());
                    }
                }
                _ => {}
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
            context_window: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            current_provider: None,
            current_model: None,
        })
    }

    // ============================================================
    // Metadata Operations
    // ============================================================

    /// Record token usage (in-memory only, persists to index via MetadataController)
    /// 
    /// `context_window` is the total_tokens from the current assistant message.
    /// `input` and `output` are the incremental tokens for this turn.
    pub fn record_usage(&mut self, context_window: usize, input: usize, output: usize) {
        self.context_window = context_window;
        self.total_input_tokens += input;
        self.total_output_tokens += output;
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
            self.total_input_tokens,
            self.total_output_tokens,
            self.context_window,
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
    /// Stores the message in LLM-native format (LlmMessageEvent with role="system")
    /// for consistent session storage.
    pub async fn add_system(&mut self, content: impl Into<String>) -> Result<()> {
        // Use native format for unified storage
        self.add_llm_message(
            "system",
            vec![ContentBlock::Text {
                text: content.into(),
            }],
            None,
            None,
            None,
        )
        .await
    }

    /// Add a user message
    ///
    /// Stores the message in LLM-native format (LlmMessageEvent) with full
    /// content block fidelity for accurate session resumption.
    pub async fn add_user(&mut self, content: impl Into<String>) -> Result<()> {
        // Use native format for unified storage
        self.add_llm_message(
            "user",
            vec![ContentBlock::Text {
                text: content.into(),
            }],
            None,
            None,
            None,
        )
        .await
    }

    /// Add an assistant message with optional tool calls
    ///
    /// Stores the message in LLM-native format (LlmMessageEvent) with full
    /// content block fidelity, preserving tool calls for accurate session resumption.
    pub async fn add_assistant(
        &mut self,
        content: impl Into<String>,
        tool_calls: Option<Vec<ToolCall>>,
        usage: Option<ProviderTokenUsage>,
    ) -> Result<()> {
        let content_str = content.into();

        // Convert ToolCall to ToolCallBlock
        let tool_call_blocks: Option<Vec<crate::session::events::ToolCallBlock>> = tool_calls.map(|calls| {
            calls
                .into_iter()
                .map(|call| ToolCallBlock {
                    id: format!("tc_{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
                    name: call.name,
                    arguments: call.parameters,
                })
                .collect()
        });

        // Build content blocks: text + tool calls
        let mut content_blocks = vec![ContentBlock::Text { text: content_str }];
        if let Some(ref calls) = tool_call_blocks {
            for call in calls {
                content_blocks.push(ContentBlock::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
            }
        }

        // Use native format for unified storage
        self.add_assistant_with_blocks(content_blocks, tool_call_blocks, None, usage)
            .await
    }

    /// Add an assistant message with tool calls (with ContentBlock tool calls)
    ///
    /// Writes as Event Format (assistant.message) for consistency with the Pekobot
    /// session specification (DATA_MODEL.md §5.3).
    /// Add a tool result
    ///
    /// Stores the tool result in LLM-native format (LlmMessageEvent with role="tool")
    /// for consistent session storage and accurate resumption.
    pub async fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        result: impl Into<String>,
    ) -> Result<()> {
        // Use native format for unified storage
        self.add_tool_result_native(tool_call_id, tool_name, result, false)
            .await
    }

    /// Add a thinking block (streaming reasoning)
    ///
    /// Stores the thinking content in LLM-native format (LlmMessageEvent with
    /// thinking block) for consistent session storage.
    pub async fn add_thinking(
        &mut self,
        thinking: impl Into<String>,
        signature: Option<String>,
    ) -> Result<()> {
        let thinking_block = crate::session::events::ThinkingBlock {
            text: thinking.into(),
            signature,
        };

        // Store as system message with thinking block
        // Note: In the future, we may want a dedicated role for thinking
        self.add_llm_message(
            "system",
            vec![], // No text content, just thinking
            None,
            Some(thinking_block),
            None,
        )
        .await
    }

    // ============================================================
    // Core LLM-Native Implementation
    // ============================================================

    /// Add an LLM-native message with full content block fidelity
    ///
    /// This is the core implementation used by all other add_* methods.
    /// It stores messages in the new unified format (SessionEvent::MessageV2) which
    /// uses SessionMessage with RoleMetadata for clean, SRP-compliant storage.
    ///
    /// This replaces the legacy LlmMessageEvent format with the new unified format
    /// that supports all message types through a single, extensible structure.
    ///
    /// # Arguments
    /// * `role` - Message role ("system", "user", "assistant", "tool")
    /// * `content_blocks` - Content blocks in native format
    /// * `_tool_calls` - Optional tool calls (for assistant messages) - stored as content blocks
    /// * `_thinking` - Optional thinking content (for reasoning models) - stored as content blocks
    /// * `usage` - Optional token usage statistics
    pub async fn add_llm_message(
        &mut self,
        role: impl Into<String>,
        content_blocks: Vec<ContentBlock>,
        _tool_calls: Option<Vec<ToolCallBlock>>,
        _thinking: Option<crate::session::events::ThinkingBlock>,
        usage: Option<ProviderTokenUsage>,
    ) -> Result<()> {
        let role_str = role.into();
        let msg_id = generate_message_id();

        // Update token usage if provided
        if let Some(ref u) = usage {
            self.record_usage(u.total as usize, u.input as usize, u.output as usize);
        }

        // Get provider/model info
        let provider = self.current_provider.clone().unwrap_or_default();
        let model = self.current_model.clone().unwrap_or_default();

        // Build content blocks: include text, tool calls, and thinking
        let mut final_content_blocks = content_blocks;

        // Add thinking block if present (stored as content block)
        if let Some(thinking) = _thinking {
            final_content_blocks.push(ContentBlock::Thinking {
                text: thinking.text,
                signature: thinking.signature,
            });
        }

        // Create the appropriate SessionMessage based on role
        let message = match role_str.as_str() {
            "user" => SessionMessage {
                envelope: EventEnvelope {
                    id: generate_event_id(),
                    ts: Utc::now(),
                },
                message: crate::types::message::LlmMessage {
                    role: crate::types::message::MessageRole::User,
                    content: final_content_blocks,
                    timestamp: Utc::now(),
                    metadata: std::collections::HashMap::new(),
                },
                message_id: msg_id.clone(),
                role_metadata: crate::session::message::RoleMetadata::User {
                    source: crate::session::events::MessageSource::User,
                },
            },
            "assistant" => {
                let token_usage = usage.as_ref().map(|u| crate::session::message::TokenUsage {
                    input_tokens: u.input as u32,
                    output_tokens: u.output as u32,
                    total_tokens: (u.input + u.output) as u32,
                }).unwrap_or(crate::session::message::TokenUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                });

                SessionMessage::assistant_with_blocks(final_content_blocks, provider, model, token_usage)
            }
            "system" => SessionMessage::system(
                final_content_blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            "tool" => {
                // For tool messages, extract tool_call_id and content from content blocks
                let mut tool_call_id = String::new();
                let mut content_parts = Vec::new();

                for block in &final_content_blocks {
                    match block {
                        ContentBlock::ToolResult { tool_call_id: id, content, .. } => {
                            tool_call_id = id.clone();
                            // Extract text from nested content
                            for c in content {
                                if let ContentBlock::Text { text } = c {
                                    content_parts.push(text.as_str());
                                }
                            }
                        }
                        ContentBlock::Text { text } => {
                            content_parts.push(text.as_str());
                        }
                        _ => {}
                    }
                }

                let content_text = content_parts.join("");
                SessionMessage::tool_result(tool_call_id, content_text)
            }
            _ => {
                // Default to user message for unknown roles
                SessionMessage::user(
                    final_content_blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                    crate::session::events::MessageSource::User,
                )
            }
        };

        let event = SessionEvent::MessageV2(message);

        self.storage.append_event(&self.id, &event).await?;
        self.last_message_id = Some(msg_id);
        self.message_count += 1;
        Ok(())
    }

    /// Add a user message (internal implementation)
    ///
    /// Convenience wrapper around `add_llm_message` for user messages.
    async fn add_user_native(&mut self, text: impl Into<String>) -> Result<()> {
        self.add_llm_message(
            "user",
            vec![ContentBlock::Text { text: text.into() }],
            None,
            None,
            None,
        )
        .await
    }

    /// Add an assistant message with content blocks
    ///
    /// Advanced method that allows passing tool calls as blocks.
    /// For simple text-only messages, use `add_assistant` instead.
    pub async fn add_assistant_with_blocks(
        &mut self,
        content_blocks: Vec<ContentBlock>,
        tool_calls: Option<Vec<crate::session::events::ToolCallBlock>>,
        thinking: Option<crate::session::events::ThinkingBlock>,
        usage: Option<ProviderTokenUsage>,
    ) -> Result<()> {
        self.add_llm_message("assistant", content_blocks, tool_calls, thinking, usage)
            .await
    }

    /// Add a tool result (internal implementation)
    ///
    /// Stores tool results as content blocks for proper reconstruction.
    async fn add_tool_result_native(
        &mut self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        result: impl Into<String>,
        is_error: bool,
    ) -> Result<()> {
        let tool_call_id_str = tool_call_id.into();
        let tool_name_str = tool_name.into();
        let result_str = result.into();

        let content_blocks = vec![ContentBlock::ToolResult {
            tool_call_id: tool_call_id_str.clone(),
            name: tool_name_str,
            content: vec![ContentBlock::Text { text: result_str }],
            is_error,
        }];

        self.add_llm_message("tool", content_blocks, None, None, None)
            .await
    }

    // ============================================================
    // History Operations
    // ============================================================

    /// Load conversation history
    ///
    /// Returns messages with full content block fidelity, preserving tool calls,
    /// thinking blocks, and other structured content. This method supports both
    /// the new LLM-native format (LlmMessageEvent) and legacy formats for
    /// backward compatibility.
    ///
    /// # Returns
    /// Vector of ChatMessage with complete ContentBlock information
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> {
        // Delegate to native loader for unified handling
        self.load_history_native().await
    }

    /// Load conversation history (internal implementation)
    ///
    /// Core implementation that handles all event formats and converts to
    /// ChatMessage with full ContentBlock fidelity.
    async fn load_history_native(&self) -> Result<Vec<ChatMessage>> {
        let events = self.storage.load_events(&self.id).await?;
        let messages: Vec<ChatMessage> = events
            .iter()
            .filter_map(|event| event_to_chat_message(event))
            .collect();

        Ok(messages)
    }

    /// Get context as text (for LLM)
    ///
    /// Uses normalized loading to support both Legacy V3 and Event Format sessions.
    pub async fn get_context_text(&self, _limit: usize) -> String {
        let entries = match self.storage.load_normalized(&self.id).await {
            Ok(e) => e,
            Err(_) => return format!("Session: {}", self.id),
        };

        let context = entries_to_context_text(&entries);

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

    // Note: Creation tests moved to SessionManager tests
    // UnifiedSession::create* methods were removed in Phase 3
    // All creation must go through SessionManager::create_session()

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

    // ====================================================================================
    // Phase 4a: Message Conversion Function Tests
    // ====================================================================================

    #[test]
    fn test_event_to_chat_message_assistant() {
        use crate::providers::MessageRole;
        use crate::session::SessionMessage;

        let event = SessionEvent::MessageV2(SessionMessage::assistant_text("Hello!", "openai", "gpt-4"));

        let msg = event_to_chat_message(&event).unwrap();
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn test_event_to_chat_message_user() {
        use crate::providers::MessageRole;
        use crate::session::SessionMessage;

        let event = SessionEvent::MessageV2(SessionMessage::user("Hi there", crate::session::events::MessageSource::User));

        let msg = event_to_chat_message(&event).unwrap();
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn test_event_to_chat_message_system() {
        use crate::providers::MessageRole;
        use crate::session::SessionMessage;

        let event = SessionEvent::MessageV2(SessionMessage::system("System prompt"));

        let msg = event_to_chat_message(&event).unwrap();
        assert_eq!(msg.role, MessageRole::System);
    }

    #[test]
    fn test_event_to_chat_message_unhandled() {
        use crate::session::events::{EventEnvelope, SessionCreatedEvent};
        use chrono::Utc;

        let event = SessionEvent::SessionCreated(SessionCreatedEvent {
            instance_id: "instance-1".to_string(),
            image_digest: "sha256:abc".to_string(),
            parent_session_id: None,
            trigger: crate::session::events::SessionTrigger::User,
            envelope: EventEnvelope {
                id: "test-4".to_string(),
                ts: Utc::now(),
            },
        });

        // SessionCreated events should be ignored
        assert!(event_to_chat_message(&event).is_none());
    }

    #[test]
    fn test_entries_to_context_text() {
        use crate::session::NormalizedEntry;
        use chrono::Utc;

        let entries = vec![
            NormalizedEntry::UserMessage {
                id: "1".to_string(),
                content: "Hello".to_string(),
                timestamp: Utc::now(),
                source: crate::session::events::MessageSource::User,
            },
            NormalizedEntry::AssistantMessage {
                id: "2".to_string(),
                content: "Hi there".to_string(),
                timestamp: Utc::now(),
                input_tokens: 10,
                output_tokens: 5,
            },
            NormalizedEntry::SystemMessage {
                content: "System info".to_string(),
                timestamp: Utc::now(),
            },
        ];

        let context = entries_to_context_text(&entries);
        assert!(context.contains("user: Hello"));
        assert!(context.contains("assistant: Hi there"));
        assert!(context.contains("system: System info"));
    }

    #[test]
    fn test_entries_to_context_text_with_tool_result() {
        use crate::session::NormalizedEntry;
        use chrono::Utc;

        let entries = vec![NormalizedEntry::ToolResult {
            tool_call_id: "1".to_string(),
            tool_name: "read_file".to_string(),
            content: "File contents".to_string(),
            is_error: false,
        }];

        let context = entries_to_context_text(&entries);
        assert!(context.contains("tool: [read_file result: File contents]"));
    }

    #[test]
    fn test_entries_to_context_text_empty_content_skipped() {
        use crate::session::NormalizedEntry;
        use chrono::Utc;

        let entries = vec![NormalizedEntry::UserMessage {
            id: "1".to_string(),
            content: "".to_string(),
            timestamp: Utc::now(),
            source: crate::session::events::MessageSource::User,
        }];

        let context = entries_to_context_text(&entries);
        assert!(context.is_empty());
    }
}
