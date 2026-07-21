//! Compatibility re-exports for the neutral `peko-message` crate.
//!
//! The message contract (`ContentBlock`, `LlmMessage`, `MessageRole`,
//! `TokenUsage`, `AgentMessage`, `MessageConverter`, `MessageContext`,
//! `SteeringProvider`, `ContextTransformer`, …) is shared by providers,
//! sessions, quota, extensions, and the agentic loop, so it lives in
//! its own crate. Internal consumers keep the historical
//! `crate::common::types::message::...` import paths; downstream crates
//! that grow out of the workspace migration will depend on `peko-message`
//! directly.

pub use peko_message::*;
