//! Shared helpers for the Task\* planning-todo tool family.

use peko_tools_core::ToolContext;

use crate::tasks::TodoStatus;

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

/// Error returned by the Task\* tools when they're invoked without a
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

#[cfg(test)]
mod tests {
    use super::*;
    use peko_tools_core::ToolContext;

    #[test]
    fn require_session_id_missing() {
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate");
        let result = require_session_id(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn require_session_id_present() {
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:default");
        let result = require_session_id(&ctx).unwrap();
        assert_eq!(result, "agent:test:cli:default");
    }

    #[test]
    fn parse_status_param_ok() {
        let v = serde_json::json!("in_progress");
        assert_eq!(parse_status_param(&v).unwrap(), TodoStatus::InProgress);
    }

    #[test]
    fn parse_status_param_invalid_string() {
        let v = serde_json::json!("done");
        assert!(parse_status_param(&v).is_err());
    }

    #[test]
    fn parse_status_param_non_string() {
        let v = serde_json::json!(42);
        assert!(parse_status_param(&v).is_err());
    }
}
