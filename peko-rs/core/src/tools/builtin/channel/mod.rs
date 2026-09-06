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

/// Build the per-caller `ChannelSend` tool for `caller_did`, wiring the
/// daemon-global channel port and (when present) the cross-runtime ctx
/// from the shared `ExtensionCore` services. This is the same wiring
/// `Agent::init_builtins_async` performs at run start, factored out so
/// non-run dispatch paths (cron `SpawnTool`) can ensure the
/// registration without booting an agent.
///
/// Returns `None` when the core has no channel port installed — the
/// tool is useless without one.
#[must_use]
pub fn build_channel_send_tool(
    core: &crate::extensions::framework::core::ExtensionCore,
    caller_did: &str,
) -> Option<std::sync::Arc<dyn peko_tools_core::Tool>> {
    let port = core.services().channel_port()?;
    let cross_ctx = core
        .services()
        .cross_runtime_a2a_ctx()
        .and_then(|ctx| std::sync::Arc::downcast::<crate::tunnel::CrossRuntimeA2aCtx>(ctx).ok());
    let tool = match cross_ctx {
        Some(ctx) => ChannelSendTool::new_with_peer(port, caller_did.to_string(), ctx),
        None => ChannelSendTool::new_local_only(port, caller_did.to_string()),
    };
    Some(std::sync::Arc::new(tool))
}
