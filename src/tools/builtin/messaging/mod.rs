//! Messaging tools
//!
//! Tools for inter-agent communication and messaging:
//! - `Agent`: Spawn sub-agents
//! - `message_tool`: General message tool
//!
//! A2A message sending moved to `crate::tunnel::a2a_send_tool` in
//! issue #31d (cycle break). Tools layer no longer depends on tunnel.

pub mod agent;
pub mod message_tool;

pub use agent::AgentTool;
pub use message_tool::{ChannelType, MessageConfig, MessageResult, MessageTool};
