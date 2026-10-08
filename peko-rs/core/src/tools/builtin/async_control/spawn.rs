//! Async action spawn tool — invoke any tool asynchronously.
//!
//! Part of the Async* family that replaces the single `task` tool.
//! Speaks to the [`AsyncRuntime`] port to dispatch via the F37 funnel.

use async_trait::async_trait;
use serde_json::json;

use peko_tools_core::traits::Tool;

use crate::tools::builtin::async_control::{SharedAsyncRuntime, SpawnRequest};

/// Spawn an async task invoking any registered tool.
pub struct AsyncSpawnAction {
    runtime: SharedAsyncRuntime,
}

impl AsyncSpawnAction {
    /// Construct with an async runtime.
    ///
    /// The runtime holds the per-agent `Weak<ToolingRuntime>`,
    /// `principal_id` internally — agents
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
impl Tool for AsyncSpawnAction {
    fn name(&self) -> &'static str {
        "Async"
    }

    fn description(&self) -> String {
        r"Invoke any tool asynchronously and return a task receipt.

The spawned task runs in the background. Use Async action status/Async action output to check
progress and read results; use Async action stop to cancel.

Parameters:
- tool: string (required) — the tool name to invoke
- params: object (required) — parameters to pass to the tool
- label: string? — optional label for the task
- wake_on_completion: boolean? — default true; deliver completion to the spawning session and start a follow-up turn when idle. false queues the completion silently for the next run.
- timeout_secs: integer? — task lifetime in seconds, default 7200. null/omit selects the default.

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
                    "description": "If true (default), the completion is pushed into the spawning session's inbox AND, when the session is idle (no run in flight), a follow-up turn is started so the agent reacts to the result. If false, the completion is pushed silently for the next run to drain (background bookkeeping). Cron schedules use their creating session, with a trunk fallback."
                },
                "timeout_secs": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Maximum lifetime of the spawned task in seconds. Defaults to 7200 (2h). Pass null/omit to use the default. Cron schedules can override per job."
                }
            },
            "required": ["tool", "params"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.execute_inner(params, None).await
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &peko_tools_core::ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        // ADR-061 follow-up: stamp the parent session PER CALL from
        // `ToolContext.session_id` — the run id on the agent-loop path,
        // the token-resolved node id (or the session-key string when
        // nodeless) on the `ExecuteTool` path. The runtime still falls
        // back to its legacy session-key cell when this is absent
        // (ctx-less in-process dispatches).
        self.execute_inner(params, Some(ctx)).await
    }
}

impl AsyncSpawnAction {
    async fn execute_inner(
        &self,
        params: serde_json::Value,
        ctx: Option<&peko_tools_core::ToolContext>,
    ) -> anyhow::Result<serde_json::Value> {
        let tool_name = params
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Async action spawn requires 'tool'"))?
            .to_string();
        let tool_params = params
            .get("params")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Async action spawn requires 'params'"))?;
        let label = params
            .get("label")
            .and_then(|v| v.as_str())
            .map(String::from);
        let wake_on_completion = params
            .get("wake_on_completion")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let timeout_secs = params.get("timeout_secs").and_then(|v| v.as_u64());

        // The runtime encapsulates the per-agent snapshot (ToolingRuntime,
        // principal_id). The tool body has no opinions about
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
            parent_session_id: ctx
                .and_then(|ctx| ctx.session_id.clone())
                .filter(|s| !s.is_empty()),
        };

        // Scheduling retains caller context without adding model-facing fields.
        let receipt = if let Some(ctx) = ctx {
            self.runtime.spawn_with_context(request, ctx).await?
        } else {
            self.runtime.spawn(request).await?
        };

        Ok(json!({
            "task_id": receipt.task_id,
            "status": "running",
            "tool": tool_name,
        }))
    }
}
