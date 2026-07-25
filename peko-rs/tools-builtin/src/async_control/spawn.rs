//! AsyncSpawn tool — invoke any tool asynchronously.
//!
//! Part of the Async* family that replaces the single `task` tool.
//! Speaks to the [`AsyncRuntime`] port to dispatch via the F37 funnel.

use async_trait::async_trait;
use serde_json::json;

use peko_tools_core::traits::Tool;

use crate::async_control::{SharedAsyncRuntime, SpawnRequest};

/// Spawn an async task invoking any registered tool.
pub struct AsyncSpawnTool {
    runtime: SharedAsyncRuntime,
}

impl AsyncSpawnTool {
    /// Construct with an async runtime.
    ///
    /// The runtime holds the per-agent `Weak<ExtensionCore>`,
    /// `principal_id`, and capabilities snapshot internally — agents
    /// construct the runtime once and share it across the Async*
    /// family. This matches the F37+F38 funnel: the runtime's
    /// `spawn` calls `AsyncExecutor::dispatch_tool` which builds the
    /// canonical funnel closure internally.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for AsyncSpawnTool {
    fn name(&self) -> &'static str {
        "AsyncSpawn"
    }

    fn description(&self) -> String {
        r"Invoke any tool asynchronously and return a task receipt.

The spawned task runs in the background. Use AsyncStatus/AsyncOutput to check
progress and read results; use AsyncStop to cancel.

Parameters:
- tool: string (required) — the tool name to invoke
- params: object (required) — parameters to pass to the tool
- label: string? — optional label for the task

Returns: { task_id, status, tool_name }"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "tool": {
                    "type": "string",
                    "description": "The tool name to invoke (e.g., 'Bash', 'Agent', 'Read')"
                },
                "params": {
                    "type": "object",
                    "description": "Parameters to pass to the tool (forwarded verbatim)"
                },
                "label": {
                    "type": "string",
                    "description": "Optional label for the task"
                },
                "wake_on_completion": {
                    "type": "boolean",
                    "description": "If true (default), push a CompletionEvent into the spawning session's inbox when the task finishes. Set false for background bookkeeping that does not need to nudge the agent's next turn (cron schedules use this)."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum lifetime of the spawned task in seconds. Defaults to 7200 (2h). Pass null/omit to use the default. Cron schedules can override per job."
                }
            },
            "required": ["tool", "params"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let tool_name = params
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("AsyncSpawn requires 'tool'"))?
            .to_string();
        let tool_params = params
            .get("params")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("AsyncSpawn requires 'params'"))?;
        let label = params
            .get("label")
            .and_then(|v| v.as_str())
            .map(String::from);
        let wake_on_completion = params
            .get("wake_on_completion")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let timeout_secs = params.get("timeout_secs").and_then(|v| v.as_u64());

        // The runtime encapsulates the per-agent snapshot (ExtensionCore,
        // principal_id, capabilities). The tool body has no opinions about
        // them — agents construct the runtime with whatever their
        // principal context requires, and the runtime handles routing
        // through the F37 canonical funnel.
        //
        // F38 alignment: the dispatch helper in the runtime uses
        // `AsyncExecutor::dispatch_tool_with_signal` internally, which
        // builds the closure that calls `core.execute_tool_via_hook(...)`.
        // Tools pre-F37 called `tool.execute(...)` directly via
        // `core.get_tool(...)`, bypassing the gate; F37 fixed that; this
        // port-trait lift preserves the F37 routing.
        let request = SpawnRequest {
            tool_name: tool_name.clone(),
            params: tool_params,
            label,
            wake_on_completion,
            timeout_secs,
        };

        // The runtime adapter wraps the per-agent ExtensionCore snap and
        // overlays the right principal_id + capabilities. We hand it a
        // minimal SpawnRequest here so the public tool API doesn't leak
        // those concepts; the adapter fills them in. (If a caller ever
        // needs to override per-call, an optional fields API can come
        // later.)
        let receipt = self.runtime.spawn(request).await?;

        Ok(json!({
            "task_id": receipt.task_id,
            "status": "running",
            "tool": tool_name,
        }))
    }
}
