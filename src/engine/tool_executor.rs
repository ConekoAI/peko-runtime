//! Tool executor for the agentic loop
//!
//! Encapsulates tool execution via ExtensionCore, including:
//! - Workspace and agent context resolution
//! - Tool execution via `runtime::execute_tool_via_core_with_context`
//! - Session recording of tool results
//! - Event emission (ToolEnd)
//! - Duration tracking

use crate::agents::prompt::renderer::HOOK_TIMEOUT;
use crate::common::types::message::{ContentBlock, LlmMessage};
use crate::engine::AgenticEvent;
use crate::extensions::framework::core::ExtensionCore;
use crate::extensions::framework::types::HookInput;
use crate::extensions::framework::HookPoint;
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
    /// * `capabilities` - Per-call capability set used by the execution gate.
    /// * `active_extensions` - Active extension IDs for the current Principal;
    ///   the gate verifies the tool's owning extension is active.
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
        capabilities: Option<Vec<String>>,
        active_extensions: Option<Vec<String>>,
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

        // F31x: PreToolUse observe-only hook. Fire-and-forget with the
        // shared 2s budget — handlers see the same ToolCall payload the
        // dispatcher will use, but their return value is ignored (loop
        // always continues to ToolExecute in v1). Soft-fails on
        // timeout (mirrors `loop_per_hook_timeout_fails_open`).
        let pre_input = HookInput::ToolCall {
            tool_name: name.clone(),
            params: arguments.clone(),
            workspace: workspace.clone(),
            agent_id: Some(agent_id.clone()),
            session_id: Some(session_id.clone()),
            caller_id: caller_id.map(str::to_string),
            principal_id: Some(principal_id.to_string()),
            principal_name: Some(principal_name.to_string()),
            capabilities: capabilities.clone(),
            active_extensions: active_extensions.clone(),
            abort_signal: None,
        };
        let pre_point = HookPoint::PreToolUse {
            tool_name: name.clone(),
        };
        let _ = tokio::time::timeout(
            HOOK_TIMEOUT,
            extension_core.invoke_hook(pre_point, pre_input),
        )
        .await;

        let (tool_result_str, tool_result_json, success) =
            match crate::engine::tool_runtime::execute_tool_via_core_with_context(
                extension_core,
                name,
                arguments.clone(),
                workspace.clone(),
                Some(agent_id.clone()),
                Some(session_id.clone()),
                caller_id.map(str::to_string),
                Some(principal_id.to_string()),
                Some(principal_name.to_string()),
                capabilities.clone(),
                active_extensions.clone(),
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

        // F31x: PostToolUse observe-only hook. Symmetric with PreToolUse
        // — handlers see the executed result but their return value is
        // ignored. Loop continues with the ToolExecute-emitted result
        // regardless.
        let post_input = HookInput::ToolCall {
            tool_name: name.clone(),
            params: arguments.clone(),
            workspace: workspace.clone(),
            agent_id: Some(agent_id.clone()),
            session_id: Some(session_id.clone()),
            caller_id: caller_id.map(str::to_string),
            principal_id: Some(principal_id.to_string()),
            principal_name: Some(principal_name.to_string()),
            capabilities: capabilities.clone(),
            active_extensions: active_extensions.clone(),
            abort_signal: None,
        };
        let post_point = HookPoint::PostToolUse {
            tool_name: name.clone(),
        };
        let _ = tokio::time::timeout(
            HOOK_TIMEOUT,
            extension_core.invoke_hook(post_point, post_input),
        )
        .await;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        // F32a: propagate `success` into the persisted JSONL record and the
        // next-iteration LLM message so resumed sessions and the in-flight
        // context distinguish a failed dispatch from a successful zero-data
        // return. (Pre-F32a both sites hardcoded `is_error: false` even on
        // a failed tool execution — see audit doc section 3 row 2.)
        let is_error = !success;

        // Add to session
        {
            let mut s = session.write().await;
            s.add_tool_result(id, name, &tool_result_str, is_error)
                .await?;
        }

        on_event(AgenticEvent::ToolEnd {
            run_id: run_id.to_string(),
            tool_id: id.clone(),
            result: tool_result_json,
            success,
            duration_ms,
        });

        let message =
            LlmMessage::tool_result(id.clone(), name.clone(), tool_result_str.clone(), is_error)
                .with_tool_call_id(id.clone());

        Ok(ToolExecutionResult { message, success })
    }
}

#[cfg(test)]
mod tests {
    // ToolExecutor tests require a full ExtensionCore + Provider setup,
    // which is better covered by integration tests in tests/ or e2e_tests/.
}
