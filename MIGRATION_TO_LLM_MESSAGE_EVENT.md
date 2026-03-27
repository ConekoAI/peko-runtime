# Migration Plan: Full Consolidation to Unified Message Architecture (Updated)

## Overview

This plan performs a **complete consolidation** of all message event types into a single, clean architecture using existing types from `types::message`. The migration follows a **dual-format strategy** to minimize risk and allow gradual rollout.

**Event Count: 13 → 10 types** (removing 4 message variants, replacing with 1 unified variant)

**Migration Strategy:** Dual-format support → Gradual rollout → Legacy removal

---

## Philosophy: Reuse, Don't Reimplement

The codebase already has a well-designed message type system in `types::message`. This plan **uses it** instead of creating parallel structures.

```rust
// src/types/message.rs - ALREADY EXISTS AND WORKS
pub enum MessageRole { System, User, Assistant, Tool }

pub enum ContentBlock { Text, Image, ToolCall, ToolResult, Thinking }

pub struct LlmMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, Value>,
}
```

---

## Key Design Decisions

### 1. Role-Specific Metadata (SRP Compliance)

Instead of a "god object" with all optional fields, we use an enum to separate role-specific concerns:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum RoleMetadata {
    User { 
        source: MessageSource 
    },
    Assistant { 
        provider: String, 
        model: String, 
        usage: TokenUsage 
    },
    System,
    Tool { 
        tool_call_id: String 
    },
}
```

**Benefits:**
- Each role has exactly the fields it needs
- Type-safe: can't accidentally use `provider` on a user message
- Clear extension path for new roles

### 2. Dual-Format Support Strategy

Rather than a "big bang" migration, we implement:

1. **Phase 1:** Code supports both old and new formats (dual-read, dual-write)
2. **Phase 2:** New sessions use new format exclusively
3. **Phase 3:** Backfill old sessions incrementally
4. **Phase 4:** Remove legacy format support

This allows gradual rollout, easy rollback, and zero downtime.

---

## Phase 1: Unified Session Message Type

### 1.1 Create Session-Specific Wrapper

Create `src/session/message.rs` that wraps the existing `LlmMessage` with session-specific metadata:

```rust
//! Unified session message - wraps types::message::LlmMessage with session context

use crate::session::events::EventEnvelope;
use crate::types::message::{ContentBlock, LlmMessage, MessageRole};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Source of a user message
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    User,        // Typed by human
    Hook,        // Injected by hook trigger
    A2a,         // Sent via event bus
    SpawnParent, // From spawning parent
}

impl Default for MessageSource {
    fn default() -> Self {
        MessageSource::User
    }
}

/// Token usage statistics
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

/// Role-specific metadata - SRP-compliant separation of concerns
/// 
/// Each role has exactly the metadata it needs. This enum is flattened
/// into SessionMessage serialization with the "role" tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum RoleMetadata {
    /// User message metadata
    User {
        source: MessageSource,
    },
    /// Assistant message metadata
    Assistant {
        provider: String,
        model: String,
        usage: TokenUsage,
    },
    /// System message metadata (none needed)
    System,
    /// Tool result metadata
    Tool {
        tool_call_id: String,
    },
}

impl RoleMetadata {
    /// Get the message role for this metadata
    pub fn role(&self) -> MessageRole {
        match self {
            RoleMetadata::User { .. } => MessageRole::User,
            RoleMetadata::Assistant { .. } => MessageRole::Assistant,
            RoleMetadata::System => MessageRole::System,
            RoleMetadata::Tool { .. } => MessageRole::Tool,
        }
    }
}

/// Unified message event for session storage
/// 
/// This replaces: UserMessageEvent, AssistantMessageEvent, SystemMessageEvent, 
/// MessageEvent, LlmMessageEvent
/// 
/// Uses SRP-compliant RoleMetadata to separate role-specific concerns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    
    /// The core message content (from types::message)
    #[serde(flatten)]
    pub message: LlmMessage,
    
    /// Message ID (unique within session)
    pub message_id: String,
    
    /// Role-specific metadata (SRP-compliant)
    #[serde(flatten)]
    pub role_metadata: RoleMetadata,
}

impl SessionMessage {
    /// Create a user message
    pub fn user(content: impl Into<String>, source: MessageSource) -> Self {
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage::user(content),
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::User { source },
        }
    }
    
    /// Create an assistant message
    pub fn assistant(
        content: Vec<ContentBlock>,
        provider: impl Into<String>,
        model: impl Into<String>,
        usage: TokenUsage,
    ) -> Self {
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage {
                role: MessageRole::Assistant,
                content,
                timestamp: Utc::now(),
                metadata: HashMap::new(),
            },
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::Assistant {
                provider: provider.into(),
                model: model.into(),
                usage,
            },
        }
    }
    
    /// Create a system message
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage::system(content),
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::System,
        }
    }
    
    /// Create a tool result message
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let tool_call_id_str = tool_call_id.into();
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: tool_call_id_str.clone(),
                    name: String::new(), // Tool name not stored at message level
                    content: vec![ContentBlock::Text { text: content.into() }],
                    is_error: false,
                }],
                timestamp: Utc::now(),
                metadata: HashMap::new(),
            },
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::Tool {
                tool_call_id: tool_call_id_str,
            },
        }
    }
    
    /// Get the message role
    pub fn role(&self) -> MessageRole {
        self.message.role
    }
    
    /// Get text content (convenience)
    pub fn text_content(&self) -> String {
        self.message
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
    
    /// Get message source (if user message)
    pub fn source(&self) -> Option<MessageSource> {
        match &self.role_metadata {
            RoleMetadata::User { source } => Some(*source),
            _ => None,
        }
    }
    
    /// Get provider (if assistant message)
    pub fn provider(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Assistant { provider, .. } => Some(provider),
            _ => None,
        }
    }
    
    /// Get model (if assistant message)
    pub fn model(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Assistant { model, .. } => Some(model),
            _ => None,
        }
    }
    
    /// Get token usage (if assistant message)
    pub fn usage(&self) -> Option<&TokenUsage> {
        match &self.role_metadata {
            RoleMetadata::Assistant { usage, .. } => Some(usage),
            _ => None,
        }
    }
    
    /// Get tool call ID (if tool message)
    pub fn tool_call_id(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Tool { tool_call_id } => Some(tool_call_id),
            _ => None,
        }
    }
    
    /// Convert to ChatMessage for provider APIs
    pub fn to_chat_message(&self) -> crate::providers::ChatMessage {
        crate::providers::ChatMessage {
            role: self.message.role,
            content: self.message.content.clone(),
            tool_calls: None, // Extract from content blocks if needed
            tool_call_id: self.tool_call_id().map(|s| s.to_string()),
        }
    }
}

fn generate_message_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', ""))
}
```

### 1.2 Update SessionEvent Enum (Dual-Format Support)

During the transition period, both old and new formats are supported:

```rust
// src/session/events.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    #[serde(rename = "session.created")]
    SessionCreated(SessionCreatedEvent),
    
    // === LEGACY FORMATS (deprecated, for backward compatibility) ===
    /// Legacy user message format (pre-migration)
    #[serde(rename = "user.message")]
    UserMessage(UserMessageEvent),
    /// Legacy assistant message format (pre-migration)
    #[serde(rename = "assistant.message")]
    AssistantMessage(AssistantMessageEvent),
    /// Legacy system message format (pre-migration)
    #[serde(rename = "system.message")]
    SystemMessage(SystemMessageEvent),
    /// Legacy unified format (pre-migration)
    #[serde(rename = "message")]
    Message(MessageEvent),
    /// Legacy LLM-native format (pre-migration)
    #[serde(rename = "llm.message")]
    LlmMessage(LlmMessageEvent),
    
    // === NEW UNIFIED FORMAT ===
    /// Unified message (replaces all above)
    #[serde(rename = "message.v2")]
    MessageV2(SessionMessage),
    
    #[serde(rename = "thinking")]
    Thinking(ThinkingEvent),
    #[serde(rename = "tool.call")]
    ToolCall(ToolCallEvent),
    #[serde(rename = "tool.result")]
    ToolResult(ToolResultEvent),
    #[serde(rename = "spawn.request")]
    SpawnRequest(SpawnRequestEvent),
    #[serde(rename = "spawn.result")]
    SpawnResult(SpawnResultEvent),
    #[serde(rename = "a2a.sent")]
    A2aSent(A2aSentEvent),
    #[serde(rename = "a2a.received")]
    A2aReceived(A2aReceivedEvent),
    #[serde(rename = "hook.trigger")]
    HookTrigger(HookTriggerEvent),
    #[serde(rename = "system")]
    System(SystemEvent),
    #[serde(rename = "session.ended")]
    SessionEnded(SessionEndedEvent),
}

impl SessionEvent {
    pub fn envelope(&self) -> &EventEnvelope {
        match self {
            SessionEvent::SessionCreated(e) => &e.envelope,
            SessionEvent::UserMessage(e) => &e.envelope,
            SessionEvent::AssistantMessage(e) => &e.envelope,
            SessionEvent::SystemMessage(e) => &e.envelope,
            SessionEvent::Message(e) => &e.envelope,
            SessionEvent::LlmMessage(e) => &e.envelope,
            SessionEvent::MessageV2(e) => &e.envelope,
            SessionEvent::Thinking(e) => &e.envelope,
            SessionEvent::ToolCall(e) => &e.envelope,
            SessionEvent::ToolResult(e) => &e.envelope,
            SessionEvent::SpawnRequest(e) => &e.envelope,
            SessionEvent::SpawnResult(e) => &e.envelope,
            SessionEvent::A2aSent(e) => &e.envelope,
            SessionEvent::A2aReceived(e) => &e.envelope,
            SessionEvent::HookTrigger(e) => &e.envelope,
            SessionEvent::System(e) => &e.envelope,
            SessionEvent::SessionEnded(e) => &e.envelope,
        }
    }
    
    pub fn event_type(&self) -> &'static str {
        match self {
            SessionEvent::SessionCreated(_) => "session.created",
            SessionEvent::UserMessage(_) => "user.message",
            SessionEvent::AssistantMessage(_) => "assistant.message",
            SessionEvent::SystemMessage(_) => "system.message",
            SessionEvent::Message(_) => "message",
            SessionEvent::LlmMessage(_) => "llm.message",
            SessionEvent::MessageV2(_) => "message.v2",
            SessionEvent::Thinking(_) => "thinking",
            SessionEvent::ToolCall(_) => "tool.call",
            SessionEvent::ToolResult(_) => "tool.result",
            SessionEvent::SpawnRequest(_) => "spawn.request",
            SessionEvent::SpawnResult(_) => "spawn.result",
            SessionEvent::A2aSent(_) => "a2a.sent",
            SessionEvent::A2aReceived(_) => "a2a.received",
            SessionEvent::HookTrigger(_) => "hook.trigger",
            SessionEvent::System(_) => "system",
            SessionEvent::SessionEnded(_) => "session.ended",
        }
    }
    
    /// Check if this is a message event (any format)
    pub fn is_message(&self) -> bool {
        matches!(self,
            SessionEvent::UserMessage(_) |
            SessionEvent::AssistantMessage(_) |
            SessionEvent::SystemMessage(_) |
            SessionEvent::Message(_) |
            SessionEvent::LlmMessage(_) |
            SessionEvent::MessageV2(_)
        )
    }
    
    /// Get message if this is a message event (any format)
    /// 
    /// Legacy formats are converted to SessionMessage on-the-fly
    pub fn as_message(&self) -> Option<SessionMessage> {
        match self {
            SessionEvent::MessageV2(m) => Some(m.clone()),
            SessionEvent::UserMessage(e) => Some(SessionMessage {
                envelope: e.envelope.clone(),
                message: LlmMessage::user(&e.content),
                message_id: e.message_id.clone(),
                role_metadata: RoleMetadata::User { source: e.source },
            }),
            SessionEvent::AssistantMessage(e) => Some(SessionMessage {
                envelope: e.envelope.clone(),
                message: LlmMessage::assistant(&e.content),
                message_id: e.message_id.clone(),
                role_metadata: RoleMetadata::Assistant {
                    provider: String::new(),
                    model: String::new(),
                    usage: EventTokenUsage {
                        input_tokens: e.usage.input_tokens,
                        output_tokens: e.usage.output_tokens,
                        total_tokens: e.usage.total_tokens,
                    },
                },
            }),
            SessionEvent::SystemMessage(e) => Some(SessionMessage {
                envelope: e.envelope.clone(),
                message: LlmMessage::system(&e.content),
                message_id: generate_message_id(),
                role_metadata: RoleMetadata::System,
            }),
            SessionEvent::Message(e) => {
                // Parse legacy MessageEvent
                let role = parse_role(&e.role)?;
                let content = parse_content(&e.content)?;
                let role_metadata = match role {
                    MessageRole::User => RoleMetadata::User { source: MessageSource::User },
                    MessageRole::Assistant => RoleMetadata::Assistant {
                        provider: String::new(),
                        model: String::new(),
                        usage: e.usage.as_ref().map(|u| EventTokenUsage {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            total_tokens: u.total_tokens,
                        }).unwrap_or(EventTokenUsage { input_tokens: 0, output_tokens: 0, total_tokens: 0 }),
                    },
                    MessageRole::System => RoleMetadata::System,
                    MessageRole::Tool => RoleMetadata::Tool {
                        tool_call_id: e.tool_call_id.clone().unwrap_or_default(),
                    },
                };
                Some(SessionMessage {
                    envelope: e.envelope.clone(),
                    message: LlmMessage { role, content, timestamp: e.envelope.ts, metadata: HashMap::new() },
                    message_id: e.envelope.id.clone(),
                    role_metadata,
                })
            }
            SessionEvent::LlmMessage(e) => {
                // Parse legacy LlmMessageEvent
                let role = parse_role(&e.role)?;
                let role_metadata = match role {
                    MessageRole::User => RoleMetadata::User { source: MessageSource::User },
                    MessageRole::Assistant => RoleMetadata::Assistant {
                        provider: e.provider.clone(),
                        model: e.model.clone(),
                        usage: e.usage.as_ref().map(|u| EventTokenUsage {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            total_tokens: u.total_tokens,
                        }).unwrap_or(EventTokenUsage { input_tokens: 0, output_tokens: 0, total_tokens: 0 }),
                    },
                    MessageRole::System => RoleMetadata::System,
                    MessageRole::Tool => RoleMetadata::Tool {
                        tool_call_id: e.tool_call_id.clone().unwrap_or_default(),
                    },
                };
                Some(SessionMessage {
                    envelope: e.envelope.clone(),
                    message: LlmMessage { role, content: e.content_blocks.clone(), timestamp: e.envelope.ts, metadata: HashMap::new() },
                    message_id: e.message_id.clone(),
                    role_metadata,
                })
            }
            _ => None,
        }
    }
    
    /// Check if this is an assistant message (any format)
    pub fn is_assistant_message(&self) -> bool {
        match self {
            SessionEvent::AssistantMessage(_) => true,
            SessionEvent::LlmMessage(e) => e.role == "assistant",
            SessionEvent::Message(e) => e.role == "assistant",
            SessionEvent::MessageV2(m) => m.role() == MessageRole::Assistant,
            _ => false,
        }
    }
    
    /// Get assistant content if this is an assistant message (any format)
    pub fn assistant_content(&self) -> Option<String> {
        match self {
            SessionEvent::AssistantMessage(e) => Some(e.content.clone()),
            SessionEvent::LlmMessage(e) if e.role == "assistant" => {
                Some(e.content_blocks.iter()
                    .filter_map(|b| match b {
                        crate::types::message::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect())
            }
            SessionEvent::Message(e) if e.role == "assistant" => Some(e.content.clone()),
            SessionEvent::MessageV2(m) if m.role() == MessageRole::Assistant => {
                Some(m.text_content())
            }
            _ => None,
        }
    }
}

fn parse_role(role_str: &str) -> Option<MessageRole> {
    match role_str {
        "system" => Some(MessageRole::System),
        "user" => Some(MessageRole::User),
        "assistant" => Some(MessageRole::Assistant),
        "tool" => Some(MessageRole::Tool),
        _ => None,
    }
}

fn parse_content(content_json: &str) -> Option<Vec<ContentBlock>> {
    serde_json::from_str(content_json).ok()
}
```

---

## Phase 2: Incremental Migration Strategy

### 2.1 Dual-Format Support Implementation

**Step 1: Deploy code with dual-format support (Week 1)**

- New code reads both old and new formats via `as_message()`
- New code writes new format (`message.v2`) exclusively
- Feature flag controls write format (default: new format)

```rust
// In UnifiedSession - always write new format
pub async fn add_user(&mut self, content: impl Into<String>) -> Result<()> {
    let message = SessionMessage::user(content, MessageSource::User);
    let event = SessionEvent::MessageV2(message);
    self.storage.append_event(&self.id, &event).await?;
    self.message_count += 1;
    Ok(())
}
```

**Step 2: Monitor and validate (Week 2)**

- Monitor for deserialization errors
- Validate that new sessions work correctly
- Test session resumption with new format

**Step 3: Backfill old sessions (Week 3-4)**

Create `src/bin/migrate_session_events.rs`:

```rust
//! Incremental migration: Convert legacy message events to unified format
//! 
//! This script rewrites session files incrementally, converting:
//! - user.message → message.v2 with role=user
//! - assistant.message → message.v2 with role=assistant
//! - system.message → message.v2 with role=system
//! - message (legacy) → message.v2
//! - llm.message → message.v2
//!
//! Usage: Run during low-traffic periods, one agent at a time

use anyhow::Result;
use std::path::Path;

#[derive(Debug, Default)]
struct MigrationStats {
    files_processed: usize,
    events_migrated: usize,
    events_skipped: usize,
    files_failed: usize,
}

async fn migrate_sessions(sessions_dir: &Path, dry_run: bool) -> Result<MigrationStats> {
    let mut stats = MigrationStats::default();
    
    // Find all .jsonl files
    let mut entries = tokio::fs::read_dir(sessions_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "jsonl") {
            match migrate_session_file(&path, dry_run).await {
                Ok((migrated, skipped)) => {
                    stats.files_processed += 1;
                    stats.events_migrated += migrated;
                    stats.events_skipped += skipped;
                }
                Err(e) => {
                    eprintln!("Failed to migrate {}: {}", path.display(), e);
                    stats.files_failed += 1;
                }
            }
        }
    }
    
    Ok(stats)
}

async fn migrate_session_file(path: &Path, dry_run: bool) -> Result<(usize, usize)> {
    use crate::session::events::SessionEvent;
    
    let content = tokio::fs::read_to_string(path).await?;
    let mut migrated_count = 0;
    let mut skipped_count = 0;
    let mut new_lines = Vec::new();
    
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        
        // Try to parse as SessionEvent
        if let Ok(event) = serde_json::from_str::<SessionEvent>(line) {
            // Check if already in new format
            if matches!(event, SessionEvent::MessageV2(_)) {
                new_lines.push(line.to_string());
                skipped_count += 1;
                continue;
            }
            
            // Try to convert legacy message formats
            if let Some(message) = event.as_message() {
                if !matches!(event, SessionEvent::MessageV2(_)) {
                    // This was a legacy format that got converted
                    let new_event = SessionEvent::MessageV2(message);
                    new_lines.push(serde_json::to_string(&new_event)?);
                    migrated_count += 1;
                    continue;
                }
            }
        }
        
        // Keep non-message events as-is
        new_lines.push(line.to_string());
        skipped_count += 1;
    }
    
    if !dry_run && migrated_count > 0 {
        // Write back atomically
        let new_content = new_lines.join("\n") + "\n";
        let temp_path = path.with_extension("tmp");
        tokio::fs::write(&temp_path, new_content).await?;
        tokio::fs::rename(&temp_path, path).await?;
    }
    
    Ok((migrated_count, skipped_count))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: migrate_session_events <sessions_dir> [--dry-run]");
        std::process::exit(1);
    }
    
    let sessions_dir = &args[1];
    let dry_run = args.contains(&"--dry-run".to_string());
    
    let path = Path::new(sessions_dir);
    if !path.exists() {
        anyhow::bail!("Sessions directory does not exist: {}", sessions_dir);
    }
    
    if dry_run {
        println!("DRY RUN - No changes will be made");
    }
    
    println!("Migrating session files in: {}", sessions_dir);
    let stats = migrate_sessions(path, dry_run).await?;
    
    println!("\nMigration complete:");
    println!("  Files processed: {}", stats.files_processed);
    println!("  Events migrated: {}", stats.events_migrated);
    println!("  Events skipped (already new format or non-message): {}", stats.events_skipped);
    println!("  Files failed: {}", stats.files_failed);
    
    Ok(())
}
```

**Step 4: Remove legacy format support (Week 5+)**

After all sessions are migrated:

```rust
// Remove from SessionEvent enum:
// - UserMessage(UserMessageEvent)
// - AssistantMessage(AssistantMessageEvent)
// - SystemMessage(SystemMessageEvent)
// - Message(MessageEvent)
// - LlmMessage(LlmMessageEvent)

// Remove from SessionEvent::as_message():
// - Legacy format conversion code

// Remove legacy type definitions from events.rs
```

### 2.2 Migration Safety Checklist

- [ ] **Backup**: Create backup of all session files before migration
- [ ] **Dry run**: Test migration with `--dry-run` first
- [ ] **Incremental**: Migrate one agent at a time
- [ ] **Monitoring**: Watch for errors during migration
- [ ] **Rollback plan**: Keep backup until verification complete
- [ ] **Verification**: Spot-check migrated sessions load correctly

---

## Phase 3: Update Consumers

### 3.1 UnifiedSession Updates

```rust
// src/session/unified.rs

impl UnifiedSession {
    /// Add a user message (writes new format)
    pub async fn add_user(&mut self, content: impl Into<String>) -> Result<()> {
        let message = SessionMessage::user(content, MessageSource::User);
        let event = SessionEvent::MessageV2(message);
        self.storage.append_event(&self.id, &event).await?;
        self.message_count += 1;
        Ok(())
    }
    
    /// Add an assistant message (writes new format)
    pub async fn add_assistant(
        &mut self,
        content: Vec<ContentBlock>,
        usage: TokenUsage,
    ) -> Result<()> {
        let provider = self.current_provider.clone().unwrap_or_default();
        let model = self.current_model.clone().unwrap_or_default();
        
        let message = SessionMessage::assistant(content, provider, model, usage);
        let event = SessionEvent::MessageV2(message);
        self.storage.append_event(&self.id, &event).await?;
        self.message_count += 1;
        Ok(())
    }
    
    /// Add a system message (writes new format)
    pub async fn add_system(&mut self, content: impl Into<String>) -> Result<()> {
        let message = SessionMessage::system(content);
        let event = SessionEvent::MessageV2(message);
        self.storage.append_event(&self.id, &event).await?;
        Ok(())
    }
    
    /// Load history as ChatMessages (reads any format)
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> {
        let events = self.storage.load_events(&self.id).await?;
        
        let messages: Vec<ChatMessage> = events
            .iter()
            .filter_map(|e| e.as_message())  // Converts legacy formats automatically
            .map(|m| m.to_chat_message())
            .collect();
        
        Ok(messages)
    }
}
```

### 3.2 Event Conversion Simplification

```rust
// src/session/jsonl.rs - normalize_event becomes simpler

fn normalize_event(event: SessionEvent) -> Option<NormalizedEntry> {
    // Use unified conversion for all message types
    if let Some(m) = event.as_message() {
        let text = m.text_content();
        match m.role() {
            MessageRole::User => Some(NormalizedEntry::UserMessage {
                id: m.message_id,
                content: text,
                timestamp: m.envelope.ts,
                source: m.source().unwrap_or(MessageSource::User),
            }),
            MessageRole::Assistant => Some(NormalizedEntry::AssistantMessage {
                id: m.message_id,
                content: text,
                timestamp: m.envelope.ts,
                input_tokens: m.usage().map(|u| u.input_tokens).unwrap_or(0),
                output_tokens: m.usage().map(|u| u.output_tokens).unwrap_or(0),
            }),
            MessageRole::System => Some(NormalizedEntry::SystemMessage {
                content: text,
                timestamp: m.envelope.ts,
            }),
            MessageRole::Tool => Some(NormalizedEntry::ToolResult {
                tool_call_id: m.tool_call_id().unwrap_or_default().to_string(),
                tool_name: String::new(),
                content: text,
                is_error: false,
            }),
        }
    } else {
        // Handle non-message events (thinking, tool.call, etc.)
        match event {
            SessionEvent::ToolResult(e) => Some(NormalizedEntry::ToolResult {
                tool_call_id: e.tool_call_id,
                tool_name: String::new(),
                content: e.output.unwrap_or_default(),
                is_error: e.error.is_some(),
            }),
            _ => None,
        }
    }
}
```

### 3.3 API Response Updates

```rust
// src/api/routes/sessions.rs

impl From<&SessionEvent> for HistoryEventResponse {
    fn from(event: &SessionEvent) -> Self {
        // Use unified conversion for all message types
        if let Some(m) = event.as_message() {
            let role = match m.role() {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
                MessageRole::Tool => "tool",
            };
            
            Self {
                id: m.envelope.id.clone(),
                event_type: "message".to_string(),
                role: Some(role.to_string()),
                content: Some(m.text_content()),
                tool: None,
                args: None,
                tool_call_id: m.tool_call_id().map(|s| s.to_string()),
                output: None,
                error: None,
                created_at: m.envelope.ts.to_rfc3339(),
            }
        } else {
            // Handle non-message events
            let envelope = event.envelope();
            let mut response = Self {
                id: envelope.id.clone(),
                event_type: event.event_type().to_string(),
                role: None,
                content: None,
                tool: None,
                args: None,
                tool_call_id: None,
                output: None,
                error: None,
                created_at: envelope.ts.to_rfc3339(),
            };
            
            // Add event-specific fields
            match event {
                SessionEvent::ToolCall(e) => {
                    response.tool = Some(e.tool.clone());
                    response.args = Some(e.args.clone());
                    response.tool_call_id = Some(e.tool_call_id.clone());
                }
                SessionEvent::ToolResult(e) => {
                    response.tool_call_id = Some(e.tool_call_id.clone());
                    response.output = e.output.clone();
                    response.error = e.error.clone();
                }
                _ => {}
            }
            
            response
        }
    }
}
```

---

## Phase 4: Cleanup

### 4.1 Remove Legacy Types (After Migration Complete)

After all sessions are migrated and verified:

```rust
// DELETE from src/session/events.rs:
// - UserMessageEvent
// - AssistantMessageEvent  
// - SystemMessageEvent
// - MessageEvent
// - LlmMessageEvent
// - SessionEvent::UserMessage variant
// - SessionEvent::AssistantMessage variant
// - SessionEvent::SystemMessage variant
// - SessionEvent::Message variant
// - SessionEvent::LlmMessage variant
// - SessionEvent::as_message() legacy conversion code
```

### 4.2 Update Module Exports

```rust
// src/session/mod.rs

pub mod message;
pub use message::{SessionMessage, MessageSource, TokenUsage, RoleMetadata};
```

### 4.3 Rename MessageV2 to Message (Final Step)

After legacy removal:

```rust
// In SessionEvent enum:
#[serde(rename = "message")]  // Was "message.v2"
Message(SessionMessage),      // Was MessageV2
```

---

## Summary of Changes

| File | Changes |
|------|---------|
| `src/session/message.rs` | **NEW** - Unified SessionMessage type with RoleMetadata |
| `src/session/events.rs` | Add MessageV2 variant, keep legacy for dual-format support |
| `src/session/unified.rs` | Use new SessionMessage constructors, dual-read support |
| `src/session/jsonl.rs` | Simplify normalize_event using as_message() |
| `src/api/routes/sessions.rs` | Update response conversion using as_message() |
| `src/bin/migrate_session_events.rs` | **NEW** - Incremental migration tool |

## Event Type Count

**Before:** 13 event types  
**During:** 14 event types (with MessageV2)  
**After:** 10 event types (legacy removed)

**Removed (after migration):**
- `user.message` → unified `message`
- `assistant.message` → unified `message`
- `system.message` → unified `message`
- `message` (legacy) → unified `message`
- `llm.message` → unified `message`

## Key Improvements

1. **True unification** - One message type for all roles
2. **SRP compliance** - RoleMetadata separates role-specific concerns
3. **DRY compliance** - Reuses `types::message::LlmMessage` and `MessageRole`
4. **Zero downtime** - Dual-format support allows gradual migration
5. **Simpler pattern matching** - Single variant with typed metadata
6. **Type safety** - Uses `MessageRole` enum, not strings
7. **Extensible** - New roles add new RoleMetadata variants

## Migration Timeline

| Week | Phase | Activities |
|------|-------|------------|
| 1 | Deploy dual-format | Deploy code, monitor for errors |
| 2 | Validate new format | Test new sessions, session resumption |
| 3-4 | Backfill | Run migration script per-agent |
| 5+ | Remove legacy | Remove legacy types and conversion code |

## Risk Mitigation

- **Incremental rollout**: One agent at a time
- **Dual-format safety**: Old code still reads new sessions (via as_message())
- **Dry-run testing**: Migration script supports --dry-run
- **Backup strategy**: Full backup before any migration
- **Rollback**: Can stop at any phase, revert to previous code

---

## Appendix: Serialization Examples

### New Format (message.v2)

```json
{
  "type": "message.v2",
  "id": "evt_abc123",
  "ts": "2024-01-15T10:30:00Z",
  "role": "assistant",
  "message_id": "msg_xyz789",
  "content": [{"type": "text", "text": "Hello!"}],
  "provider": "openai",
  "model": "gpt-4",
  "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
}
```

Note: `role` field comes from `RoleMetadata` serialization via `#[serde(flatten)]` and `#[serde(tag = "role")]`.

### User Message Example

```json
{
  "type": "message.v2",
  "id": "evt_def456",
  "ts": "2024-01-15T10:30:00Z",
  "role": "user",
  "message_id": "msg_abc123",
  "content": [{"type": "text", "text": "Hi there"}],
  "source": "user"
}
```

### Tool Result Example

```json
{
  "type": "message.v2",
  "id": "evt_ghi789",
  "ts": "2024-01-15T10:30:00Z",
  "role": "tool",
  "message_id": "msg_def456",
  "content": [{"type": "tool_result", "tool_call_id": "tc_123", "name": "read_file", "content": [{"type": "text", "text": "file contents"}], "is_error": false}],
  "tool_call_id": "tc_123"
}
```
