//! Unified provider types - pure re-export facade
//!
//! This module provides a single import point for all provider-related types.
//! It contains NO type definitions — only re-exports from canonical sources.

// Domain types (canonical source: `types::message`)
pub use crate::common::types::message::{ContentBlock, LlmMessage, MessageRole, TokenUsage};

// Provider interface types (canonical source: `providers::traits`)
pub use crate::providers::traits::{
    BlockType, ChatOptions, ChatResponse, ContentBlockId, ContentDelta, StopReason, StreamEvent,
    ToolDefinition,
};

// Configuration types
pub use crate::common::types::provider::ProviderConfig;

// Transport types
pub use crate::providers::transport::AuthConfig;
