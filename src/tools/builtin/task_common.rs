//! Shared helpers for the Task* planning-todo tool family.

use crate::session::todos::TodoStatus;
use crate::tools::core::ToolContext;
use serde_json::json;

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

/// Build a JSON error response for missing or invalid parameters.
pub fn param_error(message: impl Into<String>) -> serde_json::Value {
    json!({"error": message.into()})
}
