//! Core types for the Extension system
//!
//! This module defines the fundamental types used throughout the Extension
//! architecture, including identifiers, manifest types, and shared data structures.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

/// Unique identifier for an extension
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExtensionId(pub String);

impl ExtensionId {
    /// Create a new extension ID
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ExtensionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for ExtensionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Unique identifier for a hook registration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HookId(pub uuid::Uuid);

impl HookId {
    /// Generate a new unique hook ID
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for HookId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for HookId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Extension manifest metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    /// Unique identifier for the extension
    pub id: ExtensionId,

    /// Extension type (skill, mcp, tool, channel, etc.)
    pub extension_type: String,

    /// Human-readable name
    pub name: String,

    /// Description of what the extension does
    pub description: String,

    /// Version of the extension
    pub version: String,

    /// Path to the extension directory
    pub path: PathBuf,

    /// Additional metadata (type-specific)
    #[serde(flatten)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl ExtensionManifest {
    /// Create a new extension manifest
    pub fn new(
        id: impl Into<String>,
        extension_type: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        version: impl Into<String>,
        path: PathBuf,
    ) -> Self {
        Self {
            id: ExtensionId::new(id),
            extension_type: extension_type.into(),
            name: name.into(),
            description: description.into(),
            version: version.into(),
            path,
            metadata: HashMap::new(),
        }
    }

    /// Get a metadata value
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.metadata.get(key)
    }

    /// Set a metadata value
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.metadata.insert(key.into(), value.into());
    }
}

/// Async execution receipt returned by extensions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncReceipt {
    /// Unique task identifier
    pub task_id: String,

    /// Estimated duration in seconds (for progress estimation)
    pub estimated_duration_secs: Option<u64>,

    /// Path to the task file on disk for polling (Option 3: minimal file-based polling)
    pub task_file: Option<std::path::PathBuf>,

    /// Optional metadata
    pub metadata: Option<serde_json::Value>,
}

impl AsyncReceipt {
    /// Create a new async receipt
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            estimated_duration_secs: None,
            task_file: None,
            metadata: None,
        }
    }

    /// Set estimated duration
    #[must_use]
    pub fn with_duration(mut self, seconds: u64) -> Self {
        self.estimated_duration_secs = Some(seconds);
        self
    }

    /// Set metadata
    #[must_use]
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Status of an async task (re-exported from `async_executor` for convenience)
pub use crate::tools::async_executor::AsyncTaskStatus;

/// Result of a hook handler invocation
#[derive(Debug)]
pub enum HookResult {
    /// Continue with modified output
    Continue(HookOutput),

    /// Continue with original input (pass-through)
    PassThrough,

    /// Stop propagation, handler consumed the event
    Handled,

    /// Replace entire result with this output
    Replace(HookOutput),

    /// Error occurred during handling
    Error(anyhow::Error),
}

/// Output from a hook handler
#[derive(Debug, Clone, Default)]
pub enum HookOutput {
    /// No output
    #[default]
    Unit,

    /// Text fragment (for prompt sections)
    Text(String),

    /// Tool registration
    Tool(crate::providers::ToolDefinition),

    /// Message transformation
    Message(crate::types::message::ContentBlock),

    /// Generic JSON value
    Json(serde_json::Value),

    /// Multiple outputs
    Vec(Vec<HookOutput>),

    /// Async execution receipt (returned by `ToolExecuteAsync`)
    Receipt(AsyncReceipt),

    /// Task status (returned by `ToolCheckStatus`)
    TaskStatus(AsyncTaskStatus),

    /// Boolean result (for operations like cancel)
    Bool(bool),

    /// Vector of LlmMessages (for compaction/context hooks)
    MessageVec(Vec<crate::types::message::LlmMessage>),
}

impl HookOutput {
    /// Create empty output
    #[must_use]
    pub fn unit() -> Self {
        Self::Unit
    }

    /// Create text output
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// Create JSON output
    pub fn json(v: impl Into<serde_json::Value>) -> Self {
        Self::Json(v.into())
    }

    /// Combine multiple outputs
    #[must_use]
    pub fn combine(outputs: Vec<HookOutput>) -> Self {
        Self::Vec(outputs)
    }

    /// Convert to text if possible
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Convert to JSON if possible
    #[must_use]
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(v) => Some(v),
            _ => None,
        }
    }

    /// Convert to receipt if possible
    #[must_use]
    pub fn as_receipt(&self) -> Option<&AsyncReceipt> {
        match self {
            Self::Receipt(r) => Some(r),
            _ => None,
        }
    }

    /// Convert to task status if possible
    #[must_use]
    pub fn as_task_status(&self) -> Option<&AsyncTaskStatus> {
        match self {
            Self::TaskStatus(s) => Some(s),
            _ => None,
        }
    }

    /// Convert to bool if possible
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Create a receipt output
    #[must_use]
    pub fn receipt(receipt: AsyncReceipt) -> Self {
        Self::Receipt(receipt)
    }

    /// Create a task status output
    #[must_use]
    pub fn task_status(status: AsyncTaskStatus) -> Self {
        Self::TaskStatus(status)
    }

    /// Create a boolean output
    #[must_use]
    pub fn bool(value: bool) -> Self {
        Self::Bool(value)
    }

    /// Create a message vector output
    #[must_use]
    pub fn message_vec(messages: Vec<crate::types::message::LlmMessage>) -> Self {
        Self::MessageVec(messages)
    }
}

/// Input to a hook handler
#[derive(Debug, Clone, Default)]
pub enum HookInput {
    /// No input
    #[default]
    Unit,

    /// Prompt build state
    PromptBuild(PromptBuildState),

    /// Tool registry access
    ToolRegistry(ToolRegistryAccess),

    /// Tool call parameters
    ToolCall {
        tool_name: String,
        params: serde_json::Value,
        /// Workspace directory for tool execution (optional)
        workspace: Option<String>,
    },

    /// Async task status check
    TaskStatus { task_id: String, tool_name: String },

    /// Async task cancellation request
    TaskCancel { task_id: String, tool_name: String },

    /// Session snapshot
    SessionState(SessionSnapshot),

    /// Compaction preparation data (pre-compaction hook)
    CompactionPreparation {
        /// Messages that will be summarized
        messages_to_summarize: Vec<crate::types::message::LlmMessage>,
        /// Recent messages preserved intact (turn prefix for split turns)
        turn_prefix_messages: Vec<crate::types::message::LlmMessage>,
        /// Whether the cut landed mid-turn
        is_split_turn: bool,
        /// Previous compaction summary (for cumulative updates)
        previous_summary: Option<String>,
        /// File operations extracted from messages
        file_ops: crate::compaction::summary_format::CompactionDetails,
        /// Estimated tokens in the current context
        estimated_tokens: usize,
        /// Threshold tokens that triggered compaction
        threshold_tokens: usize,
        /// Model context window limit
        model_context_limit: usize,
        /// Compaction settings
        settings: crate::compaction::CompactionConfig,
    },

    /// Compaction result data (post-compaction hook)
    CompactionResult {
        /// Summary text from the compaction
        summary: String,
        /// Number of messages that were compacted
        messages_compacted: usize,
        /// Tokens before compaction
        tokens_before: usize,
        /// Tokens after compaction
        tokens_after: usize,
        /// Compaction number (1st, 2nd, etc.)
        compaction_number: usize,
        /// Tracked file operations from compacted messages
        details: Option<crate::compaction::summary_format::CompactionDetails>,
        /// Messages after compaction (summary + kept messages)
        messages_after: Vec<crate::types::message::LlmMessage>,
    },

    /// Message envelope
    Message(MessageEnvelope),

    /// Generic JSON value
    Json(serde_json::Value),
}

/// State during prompt building
#[derive(Debug, Clone)]
pub struct PromptBuildState {
    /// Current agent name
    pub agent_name: String,

    /// Current workspace path
    pub workspace: PathBuf,

    /// Current model
    pub model: String,

    /// Current channel
    pub channel: String,

    /// Existing sections content
    pub sections: HashMap<String, String>,
}

impl PromptBuildState {
    /// Create new prompt build state
    pub fn new(agent_name: impl Into<String>, workspace: PathBuf) -> Self {
        Self {
            agent_name: agent_name.into(),
            workspace,
            model: "default".to_string(),
            channel: "discord".to_string(),
            sections: HashMap::new(),
        }
    }

    /// Get a section's current content
    #[must_use]
    pub fn section(&self, name: &str) -> Option<&str> {
        self.sections.get(name).map(std::string::String::as_str)
    }

    /// Set a section's content
    pub fn set_section(&mut self, name: impl Into<String>, content: impl Into<String>) {
        self.sections.insert(name.into(), content.into());
    }
}

/// Access to the tool registry
#[derive(Debug, Clone)]
pub struct ToolRegistryAccess {
    /// Registered tool definitions
    pub tools: Vec<crate::providers::ToolDefinition>,
}

impl ToolRegistryAccess {
    /// Create new registry access
    #[must_use]
    pub fn new(tools: Vec<crate::providers::ToolDefinition>) -> Self {
        Self { tools }
    }

    /// Add a tool definition
    pub fn add_tool(&mut self, tool: crate::providers::ToolDefinition) {
        self.tools.push(tool);
    }
}

/// Snapshot of session state
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// Session ID
    pub session_id: String,

    /// Number of messages in session
    pub message_count: usize,

    /// Current context window size (tokens)
    pub context_tokens: usize,

    /// Session metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Message envelope for I/O hooks
#[derive(Debug, Clone)]
pub struct MessageEnvelope {
    /// Message content
    pub content: crate::types::message::ContentBlock,

    /// Source channel/entity
    pub source: Option<String>,

    /// Target channel/entity
    pub target: Option<String>,

    /// Message metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

impl MessageEnvelope {
    /// Create a new message envelope
    #[must_use]
    pub fn new(content: crate::types::message::ContentBlock) -> Self {
        Self {
            content,
            source: None,
            target: None,
            metadata: HashMap::new(),
        }
    }

    /// Set source
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Set target
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }
}

/// Priority for hook handlers (higher = earlier)
pub type HookPriority = i32;

/// Default priority for handlers
pub const DEFAULT_HOOK_PRIORITY: HookPriority = 100;

/// Priority for system handlers (highest)
pub const SYSTEM_HOOK_PRIORITY: HookPriority = 1000;

/// Priority for user handlers (normal)
pub const USER_HOOK_PRIORITY: HookPriority = 100;

/// Priority for fallback handlers (lowest)
pub const FALLBACK_HOOK_PRIORITY: HookPriority = 0;

/// Source of a tool (for metadata tracking)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Built-in tool (part of the core codebase)
    BuiltIn,
    /// MCP tool from an MCP server
    Mcp { server: String },
    /// Universal tool from an extension
    Universal { extension_id: String },
    /// General extension tool
    General { extension_id: String },
}

impl ToolSource {
    /// Get a human-readable description of the source
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            ToolSource::BuiltIn => "built-in".to_string(),
            ToolSource::Mcp { server } => format!("MCP server: {server}"),
            ToolSource::Universal { extension_id } => {
                format!("universal extension: {extension_id}")
            }
            ToolSource::General { extension_id } => format!("extension: {extension_id}"),
        }
    }
}

/// Metadata for a registered tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    /// Tool name (unique identifier)
    pub name: String,
    /// Tool description (LLM-optimized)
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
    /// Source of the tool
    pub source: ToolSource,
    /// Reserved parameters configuration
    pub reserved_params: crate::extensions::services::reserved_params::ReservedParamsConfig,
    /// Companion hook IDs registered alongside the primary execution hook.
    /// Populated by `ExtensionCore::register_tool()` and used during
    /// `unregister_tool()` for atomic cleanup.
    #[serde(skip)]
    pub companion_hook_ids: Option<Vec<HookId>>,
}

impl ToolMetadata {
    /// Create new tool metadata
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        source: ToolSource,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            source,
            reserved_params:
                crate::extensions::services::reserved_params::ReservedParamsConfig::new(),
            companion_hook_ids: None,
        }
    }

    /// Set reserved params configuration
    #[must_use]
    pub fn with_reserved_params(
        mut self,
        config: crate::extensions::services::reserved_params::ReservedParamsConfig,
    ) -> Self {
        self.reserved_params = config;
        self
    }

    /// Set companion hook IDs (used internally by `ExtensionCore::register_tool`).
    #[must_use]
    pub fn with_companion_hook_ids(mut self, ids: Vec<HookId>) -> Self {
        self.companion_hook_ids = Some(ids);
        self
    }

    /// Convert to `ToolDefinition` for LLM API
    #[must_use]
    pub fn to_tool_definition(&self) -> crate::providers::ToolDefinition {
        crate::providers::ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// Convert a `HookResult` from tool execution into a structured triplet.
///
/// Returns `(display_string, json_value, success)` where:
/// - `display_string` is the human-readable result (for LLM consumption)
/// - `json_value` is the structured result (for session storage)
/// - `success` indicates whether execution succeeded
///
/// This is the single place where `HookResult`→tool output semantics are defined,
/// ensuring `AgenticLoop` and `ToolRuntime` behave identically.
pub fn tool_result_from_hook(result: HookResult, tool_name: &str) -> (String, serde_json::Value, bool) {
    match result {
        HookResult::Continue(HookOutput::Json(result)) => {
            let s = result.to_string();
            (s, result, true)
        }
        HookResult::Continue(HookOutput::Text(result)) => {
            (result.clone(), serde_json::Value::String(result), true)
        }
        HookResult::Continue(HookOutput::Vec(outputs)) => {
            let result = outputs.iter().find_map(|o| match o {
                HookOutput::Json(v) => Some((v.to_string(), v.clone())),
                HookOutput::Text(t) => Some((t.clone(), serde_json::Value::String(t.clone()))),
                _ => None,
            });
            if let Some((s, v)) = result {
                (s, v, true)
            } else {
                let s = format!("Error: Unexpected Vec output from tool '{tool_name}'");
                (s.clone(), serde_json::Value::String(s), false)
            }
        }
        HookResult::Continue(other) => {
            let s = format!("Error: Unexpected output type from tool '{tool_name}'");
            (s.clone(), serde_json::Value::String(s), false)
        }
        HookResult::PassThrough => {
            let s = format!("Tool '{tool_name}' not available");
            (s.clone(), serde_json::Value::String(s), false)
        }
        HookResult::Error(e) => {
            let s = format!("Error: {e}");
            (s.clone(), serde_json::Value::String(s), false)
        }
        HookResult::Handled => {
            let s = format!("Error: Tool '{tool_name}' execution was consumed by handler");
            (s.clone(), serde_json::Value::String(s), false)
        }
        HookResult::Replace(output) => {
            let s = format!("Error: Tool '{tool_name}' execution was replaced: {output:?}");
            (s.clone(), serde_json::Value::String(s), false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_id() {
        let id = ExtensionId::new("test-skill");
        assert_eq!(id.0, "test-skill");
        assert_eq!(id.to_string(), "test-skill");
    }

    #[test]
    fn test_extension_manifest() {
        let manifest = ExtensionManifest::new(
            "docker-skill",
            "skill",
            "Docker Skill",
            "Manage Docker containers",
            "1.0.0",
            PathBuf::from("/tmp/skills/docker"),
        );

        assert_eq!(manifest.id.0, "docker-skill");
        assert_eq!(manifest.extension_type, "skill");
        assert_eq!(manifest.name, "Docker Skill");
    }

    #[test]
    fn test_hook_output() {
        let text = HookOutput::text("Hello");
        assert_eq!(text.as_text(), Some("Hello"));
        assert!(text.as_json().is_none());

        let json = HookOutput::json(serde_json::json!({"key": "value"}));
        assert!(json.as_text().is_none());
        assert!(json.as_json().is_some());
    }

    #[test]
    fn test_prompt_build_state() {
        let state = PromptBuildState::new("test-agent", PathBuf::from("/tmp"));
        assert_eq!(state.agent_name, "test-agent");
        assert!(state.section("tools").is_none());
    }

    #[test]
    fn test_message_envelope() {
        let envelope = MessageEnvelope::new(crate::types::message::ContentBlock::Text {
            text: "Hello".to_string(),
        })
        .with_source("user")
        .with_target("agent");

        assert_eq!(envelope.source, Some("user".to_string()));
        assert_eq!(envelope.target, Some("agent".to_string()));
    }

    #[test]
    fn test_hook_output_message_vec() {
        let messages = vec![
            crate::types::message::LlmMessage::system("System"),
            crate::types::message::LlmMessage::user("User"),
        ];
        let output = HookOutput::message_vec(messages);
        match output {
            HookOutput::MessageVec(msgs) => assert_eq!(msgs.len(), 2),
            _ => panic!("Expected MessageVec variant"),
        }
    }
}
