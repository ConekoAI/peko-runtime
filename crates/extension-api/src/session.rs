//! Session and messaging types
//!
//! Lifted from `src/extensions/framework/types/session.rs` in Phase 7.
//! The `ToolRegistryAccess.tools` field uses `peko_provider_api::ToolDefinition`
//! (was `crate::providers::ToolDefinition`); the content payload uses
//! `peko_message::ContentBlock` (was `crate::common::types::message::ContentBlock`).

use peko_message::ContentBlock;
use peko_provider_api::ToolDefinition;
use std::collections::HashMap;
use std::path::PathBuf;

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
    pub tools: Vec<ToolDefinition>,
}

impl ToolRegistryAccess {
    /// Create new registry access
    #[must_use]
    pub fn new(tools: Vec<ToolDefinition>) -> Self {
        Self { tools }
    }

    /// Add a tool definition
    pub fn add_tool(&mut self, tool: ToolDefinition) {
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
    pub content: ContentBlock,

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
    pub fn new(content: ContentBlock) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_build_state() {
        let state = PromptBuildState::new("test-agent", PathBuf::from("/tmp"));
        assert_eq!(state.agent_name, "test-agent");
        assert!(state.section("tools").is_none());
    }

    #[test]
    fn test_message_envelope() {
        let envelope = MessageEnvelope::new(ContentBlock::Text {
            text: "Hello".to_string(),
        })
        .with_source("user")
        .with_target("agent");

        assert_eq!(envelope.source, Some("user".to_string()));
        assert_eq!(envelope.target, Some("agent".to_string()));
    }
}
