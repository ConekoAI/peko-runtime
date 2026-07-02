//! Shared helpers for the Task* planning-todo tool family.

use crate::session::todos::TodoStatus;
use crate::tools::core::ToolContext;

/// Require a session id from the execution context.
pub fn require_session_id(ctx: &ToolContext) -> anyhow::Result<String> {
    ctx.session_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Task tools require a session context"))
}

/// Parse a status string from JSON parameters.
pub fn parse_status_param(value: &serde_json::Value) -> anyhow::Result<TodoStatus> {
    let s = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("status must be a string"))?;
    s.parse()
}

/// Error returned by the Task* tools when they're invoked without a
/// session context (i.e. through the bare `execute` path rather than
/// `execute_with_context`). Exposed as a single constructor so the
/// message stays consistent across the family and so callers can
/// pattern-match on it if they want to.
pub fn missing_session_error() -> anyhow::Error {
    anyhow::anyhow!(
        "Task tools require a session context; route execution through \
         execute_with_context (production callers go via ExtensionCore::invoke_hook, \
         which always supplies a session_id)"
    )
}

