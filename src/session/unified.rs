//! Session implementation
//!
//! This module provides a single, authoritative session implementation that
//! manages conversation persistence via JSONL files.
//!
//! ## Design Principles
//!
//! 1. **Single Source of Truth**: One implementation manages all session data
//! 2. **Atomic Updates**: All index updates happen together in one operation
//! 3. **Clear Ownership**: `SessionManager` is the SOLE authority for session lifecycle
//! 4. **Backward Compatible**: Works with existing session files
//!
//! ## Important: `SessionManager` is the ONLY Way
//!
//! As of Phase 3 refactor, **all session creation and opening MUST go through
//! `SessionManager`**. `Session` is now an internal implementation detail.
//! External code should use `SessionHandle` obtained from `SessionManager`.

use crate::engine::ToolCall;
use crate::providers::TokenUsage as ProviderTokenUsage;
use crate::session::events::{
    generate_event_id, generate_message_id, EventEnvelope, SessionEvent, ToolCallBlock,
};
use crate::session::jsonl::SessionStorage;
use crate::session::message::SessionMessage;
use crate::session::message_conversion::{
    entries_to_context_text, event_to_llm_message, normalized_entry_to_llm_message,
};
use crate::session::metadata_controller::MetadataController;
use crate::session::types::Peer;
use crate::session::NormalizedEntry;
use crate::types::message::LlmMessage;
use crate::types::ContentBlock;
use anyhow::Result;
use chrono::Utc;
use tracing::warn;

/// Session - internal implementation for conversation persistence
///
/// **IMPORTANT**: This is an internal implementation detail. Do not use directly.
/// All session operations should go through `SessionManager` which provides
/// `SessionHandle` for external use.
///
/// `Session` manages the JSONL file storage for conversation history.
/// It is created and opened only by `SessionManager`.
#[derive(Debug)]
pub struct Session {
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
    /// Current context window size (`total_tokens` from last assistant message)
    pub context_window: usize,
    /// Cumulative input tokens across all assistant messages
    pub total_input_tokens: usize,
    /// Cumulative output tokens across all assistant messages
    pub total_output_tokens: usize,
    /// Current provider
    pub current_provider: Option<String>,
    /// Current model
    pub current_model: Option<String>,
    /// Cached metadata controller for index updates
    metadata_controller: Option<MetadataController>,
}

impl Session {
    // ============================================================
    // Creation
    // ============================================================

    /// Create a `Session` from components (used by `SessionManager` after JSONL creation)
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
            metadata_controller: None,
        }
    }

    // ============================================================
    // Opening
    // ============================================================

    /// Open an existing unified session by ID
    ///
    /// This is the ONLY way to open a `Session`. It requires the session ID
    /// which must be obtained from `MetadataController` via `SessionManager`.
    ///
    /// NOTE: All session opening must go through `SessionManager::open_session()`.
    ///
    /// This bypasses the peer lookup and opens the session file directly.
    /// JSONL is the source of truth for message count and content.
    ///
    /// # Arguments
    /// * `agent_name` - The agent name
    /// * `session_id` - The session ID
    /// * `sessions_dir` - The sessions directory
    /// * `peer` - Optional peer info. If provided, restores session identity from this peer.
    ///            If None, defaults to `Peer::User("default`")
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

    /// Build a `Session` from normalized entries (supports both new and legacy formats)
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
            metadata_controller: None,
        })
    }

    // ============================================================
    // Metadata Operations
    // ============================================================

    /// Sync message count to index (lazy-initializes metadata controller)
    async fn sync_index_message_count(&mut self) -> Result<()> {
        if self.metadata_controller.is_none() {
            let dir = self.storage.storage_dir().to_path_buf();
            self.metadata_controller = Some(MetadataController::new(dir));
        }
        if let Some(ref mut controller) = self.metadata_controller {
            controller
                .update_message_counts(
                    &self.id,
                    self.message_count,
                    self.context_window,
                    self.total_input_tokens,
                    self.total_output_tokens,
                )
                .await?;
        }
        Ok(())
    }

    /// Record token usage (in-memory only, persists to index via `MetadataController`)
    ///
    /// `context_window` is the `total_tokens` from the current assistant message.
    /// `input` and `output` are the incremental tokens for this turn.
    pub fn record_usage(&mut self, context_window: usize, input: usize, output: usize) {
        self.context_window = context_window;
        self.total_input_tokens += input;
        self.total_output_tokens += output;
    }

    /// Set the current model (in-memory only, persists to index via `MetadataController`)
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
    /// Stores the message in LLM-native format (`LlmMessageEvent` with role="system")
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
    /// Stores the message in LLM-native format (`LlmMessageEvent`) with full
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
    /// Stores the message in LLM-native format (`LlmMessageEvent`) with full
    /// content block fidelity, preserving tool calls for accurate session resumption.
    pub async fn add_assistant(
        &mut self,
        content: impl Into<String>,
        tool_calls: Option<Vec<ToolCall>>,
        usage: Option<ProviderTokenUsage>,
    ) -> Result<()> {
        let content_str = content.into();

        // Convert ToolCall to ToolCallBlock
        let tool_call_blocks: Option<Vec<crate::session::events::ToolCallBlock>> =
            tool_calls.map(|calls| {
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

    /// Add an assistant message with tool calls (with `ContentBlock` tool calls)
    ///
    /// Writes as Event Format (assistant.message) for consistency with the Pekobot
    /// session specification (`DATA_MODEL.md` §5.3).
    /// Add a tool result
    ///
    /// Stores the tool result in LLM-native format (`LlmMessageEvent` with role="tool")
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
    /// Stores the thinking content in LLM-native format (`LlmMessageEvent` with
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
    /// It stores messages in the new unified format (`SessionEvent::MessageV2`) which
    /// uses `SessionMessage` with `RoleMetadata` for clean, SRP-compliant storage.
    ///
    /// This replaces the legacy `LlmMessageEvent` format with the new unified format
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
                    tool_call_id: None,
                },
                message_id: msg_id.clone(),
                role_metadata: crate::session::message::RoleMetadata::User {
                    source: crate::session::events::MessageSource::User,
                },
            },
            "assistant" => {
                let token_usage = usage.as_ref().map_or(
                    crate::types::message::TokenUsage {
                        input: 0,
                        output: 0,
                        total: 0,
                    },
                    |u| crate::types::message::TokenUsage {
                        input: u.input,
                        output: u.output,
                        total: u.input + u.output,
                    },
                );

                SessionMessage::assistant_with_blocks(
                    final_content_blocks,
                    provider,
                    model,
                    token_usage,
                )
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
                // For tool messages, extract tool_call_id, tool_name, and content from content blocks
                let mut tool_call_id = String::new();
                let mut tool_name = String::new();
                let mut content_parts = Vec::new();

                for block in &final_content_blocks {
                    match block {
                        ContentBlock::ToolResult {
                            tool_call_id: id,
                            name,
                            content,
                            ..
                        } => {
                            tool_call_id = id.clone();
                            tool_name = name.clone();
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
                SessionMessage::tool_result(tool_call_id, tool_name, content_text)
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

        // Update index message count
        if let Err(e) = self.sync_index_message_count().await {
            tracing::warn!("Failed to update index for session {}: {}", self.id, e);
        }

        Ok(())
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
    /// the new LLM-native format (`LlmMessageEvent`) and legacy formats for
    /// backward compatibility.
    ///
    /// # Returns
    /// Vector of `LlmMessage` with complete `ContentBlock` information
    pub async fn load_history(&self) -> Result<Vec<LlmMessage>> {
        // Delegate to native loader for unified handling
        self.load_history_native().await
    }

    /// Load conversation history (internal implementation)
    ///
    /// Core implementation that handles all event formats and converts to
    /// `LlmMessage` with full `ContentBlock` fidelity.
    async fn load_history_native(&self) -> Result<Vec<LlmMessage>> {
        let events = self.storage.load_events(&self.id).await?;
        let messages: Vec<LlmMessage> = events.iter().filter_map(event_to_llm_message).collect();

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
        details: Option<&crate::compaction::summary_format::CompactionDetails>,
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
                details,
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
    // ADR-022: Single-File Session + Derived Context Cache
    // ============================================================

    /// Append a generic event to the source-of-truth JSONL file.
    ///
    /// This is the low-level append operation. All higher-level methods
    /// (`add_user`, `add_assistant`, `record_compaction`, etc.) should
    /// ideally delegate through here for consistency.
    pub async fn append_event(&mut self, event: &SessionEvent) -> Result<()> {
        self.storage.append_event(&self.id, event).await?;
        Ok(())
    }

    /// Build the current LLM context from the source-of-truth JSONL entries.
    ///
    /// This applies compaction entries in-memory: when a `Compaction` normalized
    /// entry is encountered, only messages from that point forward are included,
    /// preceded by a system message containing the compaction summary.
    ///
    /// This is called once at session load; the result is kept in memory for the run.
    pub async fn build_context(&self) -> Result<Vec<LlmMessage>> {
        use crate::session::NormalizedEntry;

        let entries = self.storage.load_normalized(&self.id).await?;
        let mut messages = Vec::new();

        // Find the latest compaction entry
        let mut latest_compaction: Option<&NormalizedEntry> = None;
        for entry in entries.iter().rev() {
            if let NormalizedEntry::Compaction { .. } = entry {
                latest_compaction = Some(entry);
                break;
            }
        }

        if let Some(NormalizedEntry::Compaction {
            summary,
            messages_compacted,
            tokens_before,
            tokens_after,
            compaction_number,
            ..
        }) = latest_compaction
        {
            // Emit summary as a system message
            let summary_text = format!(
                "[Conversation Summary #{} — {} messages compacted, saved {} tokens]\n\n{}",
                compaction_number,
                messages_compacted,
                tokens_before.saturating_sub(*tokens_after),
                summary
            );
            messages.push(LlmMessage::system(summary_text));

            // Find the position of the compaction entry
            let compaction_idx = entries
                .iter()
                .position(|e| matches!(e, NormalizedEntry::Compaction { compaction_number: n, .. } if n == compaction_number))
                .unwrap_or(0);

            // Emit messages AFTER the compaction entry
            for entry in &entries[compaction_idx + 1..] {
                if let Some(msg) = normalized_entry_to_llm_message(entry) {
                    messages.push(msg);
                }
            }
        } else {
            // No compaction — emit all messages
            for entry in &entries {
                if let Some(msg) = normalized_entry_to_llm_message(entry) {
                    messages.push(msg);
                }
            }
        }

        Ok(messages)
    }

    /// Load current context via derived cache for fast resume.
    ///
    /// Falls back to `build_context()` if the cache is stale or missing,
    /// then writes a fresh cache for next time.
    pub async fn load_context_fast(&self) -> Result<Vec<LlmMessage>> {
        // Compute checksum and entry count from JSONL
        let checksum = self.storage.compute_jsonl_checksum(&self.id).await?;
        let entry_count = self.storage.count_jsonl_entries(&self.id).await?;

        // Try to load from cache
        if let Some(cached) = self
            .storage
            .load_context_cache(&self.id, &checksum, entry_count)
            .await?
        {
            return Ok(cached);
        }

        // Cache miss or stale — build from source of truth
        let messages = self.build_context().await?;

        // Write cache for next time
        if let Err(e) = self
            .storage
            .write_context_cache(&self.id, &messages, &checksum, entry_count)
            .await
        {
            warn!("Failed to write context cache for {}: {}", self.id, e);
        }

        Ok(messages)
    }

    /// Rewrite the derived cache after compaction.
    ///
    /// Call this after compaction produces a new message list. The cache
    /// is invalidated and rewritten with the latest JSONL checksum.
    pub async fn update_context_cache(&self, messages: &[LlmMessage]) -> Result<()> {
        let checksum = self.storage.compute_jsonl_checksum(&self.id).await?;
        let entry_count = self.storage.count_jsonl_entries(&self.id).await?;
        self.storage
            .write_context_cache(&self.id, messages, &checksum, entry_count)
            .await?;
        Ok(())
    }

    /// Invalidate the derived cache (e.g., before external modifications).
    pub async fn invalidate_context_cache(&self) -> Result<()> {
        self.storage.invalidate_context_cache(&self.id).await
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
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Creation tests moved to SessionManager tests
    // Session::create* methods were removed in Phase 3
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

    #[tokio::test]
    async fn test_load_history_preserves_tool_calls_and_results() {
        use crate::types::message::ContentBlock;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let storage = crate::session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        let peer = crate::session::types::Peer::User("default".to_string());
        let session_id = "test-session-123";

        // Create a session
        storage.create_session(session_id, None).await.unwrap();

        // Open it as a Session
        let mut session =
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap();

        // Add assistant message with text + tool call
        session
            .add_assistant_with_blocks(
                vec![
                    ContentBlock::Text {
                        text: "I'll read the file.".to_string(),
                    },
                    ContentBlock::ToolCall {
                        id: "tool_abc".to_string(),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": "test.txt"}),
                    },
                ],
                Some(vec![crate::session::events::ToolCallBlock {
                    id: "tool_abc".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "test.txt"}),
                }]),
                None,
                None,
            )
            .await
            .unwrap();

        // Add tool result
        session
            .add_tool_result("tool_abc", "read_file", "Hello World")
            .await
            .unwrap();

        // Load history
        let history = session.load_history().await.unwrap();

        // Should have: SessionCreated (skipped), Assistant, Tool
        assert_eq!(history.len(), 2, "Expected assistant + tool messages");

        // Check assistant message preserves tool call
        let assistant = &history[0];
        assert!(matches!(
            assistant.role,
            crate::types::message::MessageRole::Assistant
        ));
        assert_eq!(
            assistant.content.len(),
            2,
            "Assistant should have text + tool call"
        );
        assert!(matches!(assistant.content[0], ContentBlock::Text { .. }));
        assert!(matches!(
            assistant.content[1],
            ContentBlock::ToolCall { .. }
        ));
        if let ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } = &assistant.content[1]
        {
            assert_eq!(id, "tool_abc");
            assert_eq!(name, "read_file");
            assert_eq!(arguments, &serde_json::json!({"path": "test.txt"}));
        }

        // Check tool result preserves tool_call_id
        let tool = &history[1];
        assert!(matches!(
            tool.role,
            crate::types::message::MessageRole::Tool
        ));
        assert_eq!(tool.content.len(), 1);
        if let ContentBlock::ToolResult {
            tool_call_id,
            name,
            content,
            is_error,
        } = &tool.content[0]
        {
            assert_eq!(tool_call_id, "tool_abc");
            assert_eq!(name, "read_file");
            assert!(!(*is_error));
            assert_eq!(
                content,
                &vec![ContentBlock::Text {
                    text: "Hello World".to_string()
                }]
            );
        }
    }

    // ============================================================
    // ADR-022: build_context and load_context_fast Tests
    // ============================================================

    #[tokio::test]
    async fn test_build_context_without_compaction() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let storage = crate::session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        let peer = crate::session::types::Peer::User("default".to_string());
        let session_id = "test-build-context";

        // Create session and add messages
        storage.create_session(session_id, None).await.unwrap();
        let mut session =
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap();

        session.add_system("You are helpful.").await.unwrap();
        session.add_user("Hello").await.unwrap();
        session.add_assistant("Hi there", None, None).await.unwrap();

        let context = session.build_context().await.unwrap();
        assert_eq!(context.len(), 3);
        assert_eq!(context[0].role, crate::providers::MessageRole::System);
        assert_eq!(context[1].role, crate::providers::MessageRole::User);
        assert_eq!(context[2].role, crate::providers::MessageRole::Assistant);
    }

    #[tokio::test]
    async fn test_build_context_with_compaction() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let storage = crate::session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        let peer = crate::session::types::Peer::User("default".to_string());
        let session_id = "test-build-context-compact";

        storage.create_session(session_id, None).await.unwrap();
        let mut session =
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap();

        session.add_system("You are helpful.").await.unwrap();
        session.add_user("Old message 1").await.unwrap();
        session
            .add_assistant("Old reply", None, None)
            .await
            .unwrap();

        // Record compaction
        session
            .record_compaction("Summary of old messages", 2, 100, 20, 1, None)
            .await
            .unwrap();

        session.add_user("New message").await.unwrap();
        session
            .add_assistant("New reply", None, None)
            .await
            .unwrap();

        let context = session.build_context().await.unwrap();

        // Should have: summary system message + new user + new assistant
        assert_eq!(
            context.len(),
            3,
            "Expected summary + 2 messages after compaction"
        );
        assert_eq!(context[0].role, crate::providers::MessageRole::System);
        let summary_text = match &context[0].content[0] {
            crate::types::ContentBlock::Text { text } => text.as_str(),
            _ => "",
        };
        assert!(summary_text.contains("Summary of old messages"));
        assert_eq!(context[1].role, crate::providers::MessageRole::User);
        assert_eq!(context[2].role, crate::providers::MessageRole::Assistant);
    }

    #[tokio::test]
    async fn test_load_context_fast_uses_cache() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let peer = crate::session::types::Peer::User("default".to_string());
        let session_id = "test-fast-context";

        let storage = crate::session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        storage.create_session(session_id, None).await.unwrap();

        let mut session =
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap();

        session.add_system("You are helpful.").await.unwrap();
        session.add_user("Hello").await.unwrap();

        // First call builds from JSONL and writes cache
        let context1 = session.load_context_fast().await.unwrap();
        assert_eq!(context1.len(), 2);

        // Second call should read from cache
        let context2 = session.load_context_fast().await.unwrap();
        assert_eq!(context2.len(), 2);

        // Verify cache file exists
        assert!(session.storage.context_cache_path(session_id).exists());
    }

    #[tokio::test]
    async fn test_load_context_fast_invalidates_on_change() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let peer = crate::session::types::Peer::User("default".to_string());
        let session_id = "test-fast-context-invalidate";

        let storage = crate::session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        storage.create_session(session_id, None).await.unwrap();

        let mut session =
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap();

        session.add_system("You are helpful.").await.unwrap();
        session.add_user("Hello").await.unwrap();

        // First call writes cache
        let context1 = session.load_context_fast().await.unwrap();
        assert_eq!(context1.len(), 2);

        // Add another message (changes JSONL checksum)
        session.add_assistant("Hi!", None, None).await.unwrap();

        // Should detect stale cache and rebuild
        let context2 = session.load_context_fast().await.unwrap();
        assert_eq!(context2.len(), 3);
    }

    #[tokio::test]
    async fn test_update_context_cache() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let peer = crate::session::types::Peer::User("default".to_string());
        let session_id = "test-update-cache";

        let storage = crate::session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        storage.create_session(session_id, None).await.unwrap();

        let session = Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
            .await
            .unwrap();

        let compacted_messages = vec![
            LlmMessage::system("Summary message"),
            LlmMessage::user("Recent user msg"),
        ];

        session
            .update_context_cache(&compacted_messages)
            .await
            .unwrap();

        // Cache should be loadable
        let checksum = session
            .storage
            .compute_jsonl_checksum(session_id)
            .await
            .unwrap();
        let entry_count = session
            .storage
            .count_jsonl_entries(session_id)
            .await
            .unwrap();
        let cached = session
            .storage
            .load_context_cache(session_id, &checksum, entry_count)
            .await
            .unwrap();

        assert!(cached.is_some());
        assert_eq!(cached.unwrap().len(), 2);
    }

    // ============================================================
    // ADR-022: End-to-End Compaction Flow Test
    // ============================================================

    #[tokio::test]
    async fn test_full_compaction_flow() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let peer = crate::session::types::Peer::User("default".to_string());
        let session_id = "test-full-compaction";

        let storage = crate::session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        storage.create_session(session_id, None).await.unwrap();

        let mut session =
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap();

        // 1. Build up conversation history
        session.add_system("You are helpful.").await.unwrap();
        session.add_user("Message 1").await.unwrap();
        session.add_assistant("Reply 1", None, None).await.unwrap();
        session.add_user("Message 2").await.unwrap();
        session.add_assistant("Reply 2", None, None).await.unwrap();
        session.add_user("Message 3").await.unwrap();
        session.add_assistant("Reply 3", None, None).await.unwrap();

        // 2. Load context — should have all messages
        let context_before = session.build_context().await.unwrap();
        assert_eq!(context_before.len(), 7); // system + 6 messages

        // 3. Record a compaction
        session
            .record_compaction("Summary of old messages", 4, 500, 100, 1, None)
            .await
            .unwrap();

        // 4. Add more messages after compaction
        session.add_user("Message 4").await.unwrap();
        session.add_assistant("Reply 4", None, None).await.unwrap();

        // 5. Build context — should show summary + messages after compaction
        let context_after = session.build_context().await.unwrap();

        // Should have: summary system message + 2 new messages
        assert_eq!(
            context_after.len(),
            3,
            "Expected summary + 2 messages after compaction"
        );
        assert_eq!(
            context_after[0].role,
            crate::providers::MessageRole::System,
            "First message should be summary"
        );
        let summary_text = match &context_after[0].content[0] {
            crate::types::ContentBlock::Text { text } => text.as_str(),
            _ => "",
        };
        assert!(
            summary_text.contains("Summary of old messages"),
            "Summary should contain compaction text"
        );

        // 6. Verify context cache works after compaction
        session.update_context_cache(&context_after).await.unwrap();
        let cached = session
            .storage
            .load_context_cache(
                session_id,
                &session
                    .storage
                    .compute_jsonl_checksum(session_id)
                    .await
                    .unwrap(),
                session
                    .storage
                    .count_jsonl_entries(session_id)
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(cached.is_some(), "Cache should be valid after compaction");
        assert_eq!(
            cached.unwrap().len(),
            3,
            "Cached context should have 3 messages"
        );
    }

    #[tokio::test]
    async fn test_append_event_and_build_context() {
        use crate::session::events::{EventEnvelope, SessionCreatedEvent, SessionTrigger};
        use chrono::Utc;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let peer = crate::session::types::Peer::User("default".to_string());
        let session_id = "test-append-event";

        let storage = crate::session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        storage.create_session(session_id, None).await.unwrap();

        let mut session =
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap();

        // Append a custom event via the low-level API
        let event = crate::session::events::SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: "test-event".to_string(),
                ts: Utc::now(),
            },
            instance_id: session_id.to_string(),
            image_digest: "sha256:test".to_string(),
            parent_session_id: None,
            trigger: SessionTrigger::User,
        });

        session.append_event(&event).await.unwrap();

        // Build context — SessionCreated should be ignored
        let context = session.build_context().await.unwrap();
        assert_eq!(
            context.len(),
            0,
            "SessionCreated events should not appear in context"
        );
    }
}
