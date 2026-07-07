//! System prompt generation and management
//!
//! Each agent's system prompt is a single Markdown body (see
//! [`crate::agents::agent_config::AgentConfig::prompt`]). At build
//! time `SystemPromptService` runs the body through
//! `SystemPromptBuilder` which replaces `{{placeholder}}` tokens
//! with rendered sections (tools, skills, agents, runtime,
//! self-update).

pub mod builder;
pub mod memory;
pub mod placeholder;
pub mod service;

pub use builder::{PromptMode, SystemPromptBuilder};
pub use memory::{
    directory_from_tool_params, discover_shared_context, load_principal_memory,
    PRINCIPAL_MEMORY_FILE, SHARED_CONTEXT_FILE,
};
pub use service::SystemPromptService;
