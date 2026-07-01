//! Messaging tools
//!
//! Tools for inter-agent communication and messaging:
//! - `Agent`: Spawn sub-agents
//! - `message_tool`: General message tool
//!
//! `principal_send` (principal-level cross-runtime) lives in
//! `crate::tunnel::principal_send_tool`. Tools layer no longer
//! depends on tunnel.

pub mod agent;
pub mod message_tool;

pub use agent::{AgentTool, DynamicSessionKeyProvider};
pub use message_tool::{ChannelType, MessageConfig, MessageResult, MessageTool};
