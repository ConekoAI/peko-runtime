//! `peko_channel_read` / `peko_channel_send` — channel read + send tools.
//!
//! These are the agentic-loop entry points for PR-4a (read) and PR-5c
//! (send). The principal's agentic loop calls either on demand; the
//! audit ring buffer (PR-3c) observes every event regardless of
//! whether a tool fires. No daemon-side cross-principal reach — the
//! principal invokes the tool itself, so the boundary model stays
//! intact.
//!
//! ## Implementation
//!
//! Both are thin wrappers around the [`peko_channel::ChannelPort`]
//! trait (`peek` / `post` respectively). They pull `PrincipalId` out
//! of the [`ToolContext`] and use it as the `sender` argument, so the
//! principal boundary is enforced at the port call site (which has
//! its own `NotMember` check).
//!
//! The capability gate is the standard `tool:ChannelRead` /
//! `tool:ChannelSend` grant that the principal's capability set
//! already enforces through the F37 funnel — these tools themselves
//! do not check capabilities, the gate sits at execute-time on the
//! caller's side.

pub mod channel_read;
pub mod channel_send;
pub use channel_read::ChannelReadTool;
pub use channel_send::{
    ChannelSendArgs, ChannelSendResult, ChannelSendTool, CHANNEL_SEND_TOOL_NAME,
};