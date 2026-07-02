//! Messaging tools
//!
//! Tools for inter-agent communication and messaging:
//! - `Agent`: Spawn sub-agents
//!
//! `principal_send` (principal-level cross-runtime) lives in
//! `crate::tunnel::principal_send_tool`. Tools layer no longer
//! depends on tunnel.

pub mod agent;

pub use agent::{AgentTool, DynamicSessionKeyProvider};
