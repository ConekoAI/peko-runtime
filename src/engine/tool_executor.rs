//! Tool executor for the agentic loop
//!
//! Encapsulates tool execution via ExtensionCore, including:
//! - Workspace and agent context resolution
//! - Tool execution via `runtime::execute_tool_via_core_with_context`
//! - Session recording of tool results
//! - Event emission (ToolEnd)
//! - Duration tracking

use crate::common::types::message::{ContentBlock, LlmMessage};
use crate::engine::AgenticEvent;
use crate::extensions::framework::ExtensionCore;
use crate::session::Session;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Result of executing a single tool call.
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    /// The tool result message to append to the conversation.
    pub message: LlmMessage,
    /// Whether execution succeeded.
    pub success: bool,
}

/// Executes tool calls within the agentic loop.
///
/// This struct encapsulates the tool execution logic so the loop body
/// only iterates over tool calls and appends results.
pub struct ToolExecutor;

impl ToolExecutor {
    /// Execute a single tool call.
    ///
    /// # Arguments
    /// * `tool_call` - The `ContentBlock::ToolCall` to execute
    /// * `extension_core` - The extension core for tool dispatch
    /// * `agent_name` - The agent's name (for workspace resolution)
    /// * `agent_workspace` - The agent's configured workspace
    /// * `session` - The session for recording results
    /// * `run_id` - The current run ID (for events)
    /// * `caller_id` - The resolved caller identity (pekohub sub, API key id,
    ///   or `None` for local CLI invocations) — propagated to the tool via
    ///   `HookInput::ToolCall::caller_id` for per-user permission checks and
    ///   audit logging (issue #17).
    /// * `principal_id` - Principal scope for extension-scoped tool state.
    ///   Required in the agentic-loop path; converted back to an option only
    ///   at the `HookInput::ToolCall` boundary for legacy/standalone callers.
    /// * `principal_name` - Human-readable Principal name for Principal-scoped
    ///   tools (e.g. cron).
    /// * `allowed_extensions` - Per-call allowlist used by the execution gate
    ///   instead of the mutable global `tool_config`.
    /// * `cancel` - Soft-interrupt `CancellationToken` (PR #128). Bridged
    ///   inside `execute_tool_via_core_with_context` into the tool
    ///   layer's `AbortSignal` so the trait-default `is_aborted()`
    ///   check works in production.
    /// * `on_event` - Event callback
    ///
    /// Returns the tool result message and success flag.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        tool_call: &ContentBlock,
        extension_core: &Arc<ExtensionCore>,
        agent_name: &str,
        agent_workspace: Option<&std::path::PathBuf>,
        session: &Arc<RwLock<Session>>,
        run_id: &str,
        caller_id: Option<&str>,
        principal_id: &str,
        principal_name: &str,
        allowed_extensions: Option<Vec<String>>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        on_event: &(dyn Fn(AgenticEvent) + Send + Sync),
    ) -> Result<ToolExecutionResult> {
        let (id, name, arguments) = match tool_call {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => (id, name, arguments),
            _ => {
                return Err(anyhow::anyhow!(
                    "ToolExecutor::execute called with non-ToolCall content block"
                ));
            }
        };

        info!("Executing tool: {} (id: {})", name, id);

        let start_time = std::time::Instant::now();

        let workspace = agent_workspace.map(|p| p.to_string_lossy().to_string());
        let agent_id = agent_name.to_string();

        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };

        let (tool_result_str, tool_result_json, success) =
            match crate::engine::tool_runtime::execute_tool_via_core_with_context(
                extension_core,
                name,
                arguments.clone(),
                workspace,
                Some(agent_id),
                Some(session_id),
                caller_id.map(str::to_string),
                Some(principal_id.to_string()),
                Some(principal_name.to_string()),
                allowed_extensions,
                cancel,
            )
            .await
            {
                Ok((s, v, ok)) => {
                    if ok {
                        info!("Tool '{}' executed successfully via ExtensionCore", name);
                    }
                    (s, v, ok)
                }
                Err(e) => {
                    warn!("Tool '{}' failed via ExtensionCore: {}", name, e);
                    let s = format!("Error: {e}");
                    (s.clone(), serde_json::Value::String(s), false)
                }
            };

        let duration_ms = start_time.elapsed().as_millis() as u64;

        // Add to session
        {
            let mut s = session.write().await;
            s.add_tool_result(id, name, &tool_result_str).await?;
        }

        on_event(AgenticEvent::ToolEnd {
            run_id: run_id.to_string(),
            tool_id: id.clone(),
            result: tool_result_json,
            success,
            duration_ms,
        });

        let message = LlmMessage::tool_result(id.clone(), name.clone(), tool_result_str.clone())
            .with_tool_call_id(id.clone());

        Ok(ToolExecutionResult { message, success })
    }
}

#[cfg(test)]
mod tests {
    // ToolExecutor tests require a full ExtensionCore + Provider setup,
    // which is better covered by integration tests in tests/ or e2e_tests/.
}
