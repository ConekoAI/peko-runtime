//! Shared utilities for Tool proxy implementations
//!
//! Provides common functionality for wrapping external tools (MCP, Universal, etc.)
//! with consistent context handling, abort/timeout checks, and progress reporting.

use crate::tools::{ToolContext, ToolError};
use serde_json::Value;
use std::time::Instant;

/// Result of executing a tool with context handling
pub struct ContextExecutionResult {
    pub result: anyhow::Result<Value>,
    pub duration_ms: u64,
    pub was_aborted: bool,
}

/// Executes a tool function with full context handling (abort, timeout, progress reporting)
///
/// This helper eliminates duplication between different tool proxy implementations
/// by providing a standardized wrapper for:
/// - Pre-execution abort check
/// - Pre-execution timeout check
/// - Start progress reporting
/// - Execution with timing
/// - Post-execution abort check
/// - Post-execution timeout check
/// - Completion/failure progress reporting
///
/// # Arguments
/// * `ctx` - The tool context for abort/timeout/progress
/// * `tool_name` - Name of the tool for logging/reporting
/// * `server_name` - Optional server name for MCP tools
/// * `execute_fn` - The actual tool execution logic
///
/// # Example
/// ```rust,ignore
/// let result = execute_with_context_handling(
///     ctx,
///     "my_tool",
///     Some("mcp-server"),
///     || async { 
///         // Actual execution logic
///         Ok(json!({"result": "success"}))
///     }
/// ).await;
/// ```
pub async fn execute_with_context_handling<F, Fut>(
    ctx: &ToolContext,
    tool_name: &str,
    server_name: Option<&str>,
    execute_fn: F,
) -> anyhow::Result<Value>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    // Check abort before starting
    if ctx.is_aborted() {
        return Err(ToolError::Aborted.into());
    }

    // Check timeout before starting
    let start_time = Instant::now();
    ctx.check_timeout(start_time)?;

    // Report start status
    let start_msg = if let Some(server) = server_name {
        format!("Starting {} (via {})", tool_name, server)
    } else {
        format!("Starting {}", tool_name)
    };
    ctx.report_status(start_msg).await;

    // Execute the tool
    let result = execute_fn().await;

    // Check abort after completion
    if ctx.is_aborted() {
        return Err(ToolError::Aborted.into());
    }

    // Check timeout after completion
    ctx.check_timeout(start_time)?;

    // Report completion status
    match &result {
        Ok(_) => {
            let complete_msg = if let Some(server) = server_name {
                format!("Completed {} (via {})", tool_name, server)
            } else {
                format!("Completed {}", tool_name)
            };
            ctx.report_status(complete_msg).await;
        }
        Err(e) => {
            let fail_msg = if let Some(server) = server_name {
                format!("Failed {} (via {}): {}", tool_name, server, e)
            } else {
                format!("Failed {}: {}", tool_name, e)
            };
            ctx.report_status(fail_msg).await;
        }
    }

    result
}

/// Format a status message for tool execution
pub fn format_status(tool_name: &str, server_name: Option<&str>, status: &str) -> String {
    if let Some(server) = server_name {
        format!("{} {} (via {})", status, tool_name, server)
    } else {
        format!("{} {}", status, tool_name)
    }
}

/// Estimate tool duration based on name heuristics
///
/// This is a shared implementation used by both MCP and Universal tool proxies.
pub fn estimate_tool_duration(name: &str) -> u64 {
    let name_lower = name.to_lowercase();

    // Fast operations (milliseconds)
    if name_lower.contains("read")
        || name_lower.contains("get")
        || name_lower.contains("list")
        || name_lower.contains("search")
        || name_lower.contains("find")
    {
        return 500; // 500ms
    }

    // Medium operations (seconds)
    if name_lower.contains("write")
        || name_lower.contains("create")
        || name_lower.contains("update")
        || name_lower.contains("delete")
        || name_lower.contains("copy")
        || name_lower.contains("move")
    {
        return 2000; // 2s
    }

    // Slow operations (network/external calls)
    if name_lower.contains("fetch")
        || name_lower.contains("download")
        || name_lower.contains("upload")
        || name_lower.contains("browser")
        || name_lower.contains("http")
        || name_lower.contains("request")
    {
        return 5000; // 5s
    }

    // Very slow operations (builds, long processes)
    if name_lower.contains("build")
        || name_lower.contains("compile")
        || name_lower.contains("test")
        || name_lower.contains("run")
        || name_lower.contains("exec")
        || name_lower.contains("shell")
    {
        return 30000; // 30s
    }

    // Default
    1000 // 1s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tool_duration() {
        assert_eq!(estimate_tool_duration("read_file"), 500);
        assert_eq!(estimate_tool_duration("search_code"), 500);
        assert_eq!(estimate_tool_duration("write_file"), 2000);
        assert_eq!(estimate_tool_duration("fetch_url"), 5000);
        assert_eq!(estimate_tool_duration("build_project"), 30000);
        assert_eq!(estimate_tool_duration("unknown"), 1000);
    }

    #[test]
    fn test_format_status() {
        assert_eq!(
            format_status("my_tool", Some("mcp-server"), "Starting"),
            "Starting my_tool (via mcp-server)"
        );
        assert_eq!(
            format_status("my_tool", None, "Completed"),
            "Completed my_tool"
        );
    }
}
