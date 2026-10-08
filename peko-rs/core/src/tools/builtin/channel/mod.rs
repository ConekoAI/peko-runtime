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
//! ADR-066 P2: no capability gate — the membership check at the
//! port call site is the boundary; these tools do not gate further.

pub mod channel_read;
pub mod channel_send;
pub use channel_read::ChannelReadTool;
pub use channel_send::{
    ChannelSendArgs, ChannelSendResult, ChannelSendTool, CHANNEL_SEND_TOOL_NAME,
};

/// Build the principal-scoped `ChannelSend` tool for `caller_did`, using
/// the shared channel port and resolving the current cross-runtime context
/// per invocation. Caller identity and reply locks survive tunnel changes.
///
/// Returns `None` when the runtime has no channel port installed — the
/// tool is useless without one.
#[must_use]
pub fn build_channel_send_tool(
    tooling: &crate::tools::runtime::ToolingRuntime,
    caller_did: &str,
) -> Option<std::sync::Arc<dyn peko_tools_core::Tool>> {
    let port = tooling.services().channel_port()?;
    let tool = ChannelSendTool::new_local_only(port, caller_did.to_string())
        .with_services(std::sync::Arc::downgrade(tooling.services()));
    Some(std::sync::Arc::new(tool))
}
