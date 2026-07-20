//! AsyncSpawn tool — invoke any tool asynchronously.
//!
//! Part of the Async* family that replaces the single `task` tool.
//! Requires an `AsyncExecutor` and an `ExtensionCore` reference.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::extensions::framework::async_exec::executor::{AsyncExecutor, AsyncToolConfig};
use crate::extensions::framework::core::ExtensionCore;
use crate::subject::PrincipalId;
use crate::tools::core::Tool;

/// Spawn an async task invoking any registered tool.
pub struct AsyncSpawnTool {
    executor: Arc<AsyncExecutor>,
    extension_core: std::sync::Weak<ExtensionCore>,
    /// Agent identity (DID) used to look up this agent's session key on the
    /// shared `ExtensionCore` for `parent_session_key` stamping.
    agent_id: Option<String>,
    /// F37: snapshot of the spawning principal's ID. Used by
    /// `execute_tool_via_hook` (the canonical funnel) so the capability
    /// gate at `registry.rs:260-277` evaluates against the spawning
    /// principal, not the system principal. Captured at construction
    /// time — revocation between init and spawn is NOT observed (deferred
    /// to F37x; see audit row 7 entry).
    principal_id: PrincipalId,
    /// F37: snapshot of the spawning principal's capability grants.
    /// Same lifetime as `principal_id` — captured at construction,
    /// consumed at spawn time when the factory closure calls
    /// `core.execute_tool_via_hook(...)`.
    capabilities: Arc<Vec<String>>,
}

impl AsyncSpawnTool {
    /// Construct with executor + extension core + principal identity.
    ///
    /// `agent_id` is this tool's owning agent (typically the
    /// `Agent::identity.did`). It is used to look up the *correct* session key
    /// on the shared `ExtensionCore` so concurrent agents in daemon mode do not
    /// stamp each other's `parent_session_key` (issue #68).
    ///
    /// F37: `principal_id` + `capabilities` are snapshotted at
    /// construction. They flow into `core.execute_tool_via_hook(...)` when
    /// the factory closure runs the spawned tool dispatch, ensuring the
    /// capability gate at `registry.rs:260-277` evaluates against the
    /// spawning principal's grants. Pre-F37, `tool.execute(...)` was
    /// called directly via `core.get_tool(name)`, bypassing the gate
    /// entirely.
    #[must_use]
    pub fn new(
        executor: Arc<AsyncExecutor>,
        extension_core: std::sync::Weak<ExtensionCore>,
        agent_id: Option<String>,
        principal_id: PrincipalId,
        capabilities: Arc<Vec<String>>,
    ) -> Self {
        Self {
            executor,
            extension_core,
            agent_id,
            principal_id,
            capabilities,
        }
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
        let core = match self.extension_core.upgrade() {
            Some(c) => c,
            None => {
                return Ok(json!({
                    "error": "ExtensionCore has been dropped; cannot spawn"
                }));
            }
        };

        let tool_name = params
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("AsyncSpawn requires 'tool'"))?;
        let tool_params = params
            .get("params")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("AsyncSpawn requires 'params'"))?;
        let label = params
            .get("label")
            .and_then(|v| v.as_str())
            .map(String::from);
        // Per-call `wake_on_completion` overrides the `AsyncToolConfig`
        // default (`true`). Cron schedules override to `false` because
        // a scheduled run should not yank the agent into a fresh turn
        // unless the user explicitly asked for it.
        let wake_on_completion = params
            .get("wake_on_completion")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        // Per-call `timeout_secs` overrides the 2h default.
        let timeout_secs = params.get("timeout_secs").and_then(|v| v.as_u64());

        let task_id = format!("{}:{}", tool_name, uuid::Uuid::new_v4());

        // Look up the session key for *this* tool's owning agent (issue #68).
        let session_key = match self.agent_id.as_deref() {
            Some(agent_id) => core
                .current_session_key(agent_id)
                .unwrap_or_else(|| "unknown".to_string()),
            None => "unknown".to_string(),
        };

        let config = AsyncToolConfig {
            timeout_secs,
            label,
            wake_on_completion,
            ..Default::default()
        };

        // F37: route the inner tool dispatch through the canonical
        // funnel `core.execute_tool_via_hook(...)` instead of grabbing
        // `Arc<dyn Tool>` via `core.get_tool(...)` and calling
        // `tool.execute(...)` directly. The funnel fires the capability
        // gate at `registry.rs:260-277` and the
        // `PreToolUse` / `ToolExecute` / `PostToolUse` hook chain. Pre-F37
        // this code path skipped both.
        //
        // We can't reuse the `Arc<dyn Tool>` directly inside the factory
        // closure (it's `'static`-bound) — the core IS, so we capture a
        // cloned `Arc<ExtensionCore>` and the closure calls back into it.
        // `core.execute_tool_via_hook` returns `Result<(text, json,
        // success)>`; the factory must return `Result<Value>`, so we
        // unwrap the triplet — on failure we surface a JSON error so
        // the executor's `AsyncTaskRecord` records `is_error: true` via
        // its existing error-passthrough path.
        let core_for_closure = core.clone();
        let principal_id_for_closure = self.principal_id.0.clone();
        let capabilities_for_closure = (*self.capabilities).clone();
        let tool_name_for_closure = tool_name.to_string();
        let tool_params_for_closure = tool_params.clone();

        let receipt = self
            .executor
            .execute(
                task_id.clone(),
                tool_name,
                tool_params,
                session_key,
                config,
                move || async move {
                    let (_text, json, success) = core_for_closure
                        .execute_tool_via_hook(
                            &tool_name_for_closure,
                            tool_params_for_closure,
                            None,
                            None,
                            None,
                            None,
                            Some(principal_id_for_closure),
                            None,
                            Some(capabilities_for_closure),
                            None,
                        )
                        .await?;
                    if success {
                        Ok(json)
                    } else {
                        // Gate failure / tool error — return Err so the
                        // executor records `AsyncTaskStatus::Failed { error }`
                        // (not `Completed` with error-data JSON). The
                        // `tool_result_from_hook` translation has already
                        // populated `_text` with the user-facing error.
                        // The error string flows into the executor's task
                        // record + propagates to the outer tool caller
                        // via `?` below.
                        Err(anyhow::anyhow!("{}", _text))
                    }
                },
            )
            .await?;

        Ok(json!({
            "task_id": receipt.task_id,
            "status": "running",
            "tool": tool_name,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::async_exec::executor::AsyncExecutor;
    use serde_json::json;
    use std::sync::Arc;

    /// Minimal stub tool used only to register an entry in the
    /// ExtensionCore side-table so `AsyncSpawnTool` can resolve it.
    struct StubTool;

    #[async_trait::async_trait]
    impl crate::tools::core::Tool for StubTool {
        fn name(&self) -> &str {
            "stub_tool"
        }
        fn description(&self) -> String {
            "stub tool for tests".to_string()
        }
        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(json!({"ok": true}))
        }
    }

    /// Helper: build an AsyncSpawnTool with the post-F37 fields filled in
    /// with sensible defaults. Tests that don't care about the
    /// gate-bypass behavior just call this; the new gate tests pass
    /// their own `capabilities` and `principal_id`.
    fn make_tool(
        executor: Arc<AsyncExecutor>,
        core: &Arc<ExtensionCore>,
        agent_id: Option<String>,
    ) -> AsyncSpawnTool {
        AsyncSpawnTool::new(
            executor,
            Arc::downgrade(core),
            agent_id,
            PrincipalId::system().clone(),
            Arc::new(Vec::new()), // no capabilities — gate will reject any tool call
        )
    }

    #[tokio::test]
    async fn test_async_spawn_gate_rejects_unknown_tool() {
        let executor = Arc::new(AsyncExecutor::new());
        let core = Arc::new(ExtensionCore::new());
        // Hold `core` strongly so the Weak inside AsyncSpawnTool can
        // upgrade. `make_tool` takes `&Arc<ExtensionCore>` (not by value)
        // so the test still owns its strong reference.
        //
        // F37: the closure returns `Err(anyhow!(...))` on gate failure,
        // so the executor records `AsyncTaskStatus::Failed { error }`.
        // The outer `AsyncSpawnTool::execute` still returns Ok with
        // the task_id — `AsyncExecutor::execute_inner` always returns
        // Ok(receipt) and dispatches the closure in the background.
        // To verify the gate fired, poll the registry for the
        // task's terminal status.
        let tool = make_tool(executor.clone(), &core, None);

        let result = tool
            .execute(json!({"tool": "definitely_not_a_tool", "params": {}}))
            .await
            .unwrap();
        let task_id = result["task_id"]
            .as_str()
            .expect("task_id present")
            .to_string();

        for _ in 0..50 {
            let entry_opt = {
                let reg = executor.registry().read().await;
                reg.get(&task_id).cloned()
            };
            if let Some(entry) = entry_opt {
                match &entry.status {
                    crate::extensions::framework::async_exec::executor::AsyncTaskStatus::Failed {
                        error,
                    } => {
                        assert!(
                            error.contains("definitely_not_a_tool")
                                && error.contains("disabled"),
                            "expected gate to reject unknown tool, got: {error}"
                        );
                        return;
                    }
                    other => {
                        if matches!(
                            other,
                            crate::extensions::framework::async_exec::executor::AsyncTaskStatus::Pending
                                | crate::extensions::framework::async_exec::executor::AsyncTaskStatus::Running
                        ) {
                            // fall through to sleep
                        } else {
                            panic!(
                                "expected Failed status from gate, got terminal status: {other:?}"
                            );
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("spawned task {task_id} never recorded an outcome");
    }

    #[tokio::test]
    async fn test_async_spawn_stamps_correct_agent_session_key() {
        let core = Arc::new(ExtensionCore::new());
        core.insert_tool_instance("stub_tool".to_string(), Arc::new(StubTool))
            .await;

        let agent_a = "did:peko:agent:A";
        let agent_b = "did:peko:agent:B";
        core.set_session_key(agent_a, Some("sess-A".to_string()))
            .await;
        core.set_session_key(agent_b, Some("sess-B".to_string()))
            .await;

        let executor = Arc::new(AsyncExecutor::new());
        // F37: with `tool:stub_tool` granted, the gate passes — the
        // factory closure's tool call dispatches successfully and the
        // executor records the spawned task.
        let tool_a = AsyncSpawnTool::new(
            executor.clone(),
            Arc::downgrade(&core),
            Some(agent_a.to_string()),
            PrincipalId::system().clone(),
            Arc::new(vec!["tool:stub_tool".to_string()]),
        );

        let result = tool_a
            .execute(json!({
                "tool": "stub_tool",
                "params": {"x": 1},
            }))
            .await
            .unwrap();

        let task_id = result["task_id"].as_str().expect("task_id present");
        assert!(task_id.starts_with("stub_tool:"));

        let registry = executor.registry();
        let reg = registry.read().await;
        let entry = reg
            .get(&task_id.to_string())
            .expect("spawned task should be in the executor's registry");
        assert_eq!(entry.parent_session_key, "sess-A");
    }

    #[tokio::test]
    async fn test_async_spawn_without_agent_id_falls_back_to_unknown() {
        let core = Arc::new(ExtensionCore::new());
        core.insert_tool_instance("stub_tool".to_string(), Arc::new(StubTool))
            .await;

        let executor = Arc::new(AsyncExecutor::new());
        // F37: grant `tool:stub_tool` so the gate passes.
        let tool = AsyncSpawnTool::new(
            executor.clone(),
            Arc::downgrade(&core),
            None,
            PrincipalId::system().clone(),
            Arc::new(vec!["tool:stub_tool".to_string()]),
        );

        let result = tool
            .execute(json!({
                "tool": "stub_tool",
                "params": {"x": 1},
            }))
            .await
            .unwrap();

        let task_id = result["task_id"].as_str().expect("task_id present");
        let registry = executor.registry();
        let reg = registry.read().await;
        let entry = reg.get(&task_id.to_string()).expect("task registered");
        assert_eq!(entry.parent_session_key, "unknown");
    }

    // F37: pin that the capability gate fires for dispatched tool calls.
    // Pre-F37, `AsyncSpawnTool` skipped the gate entirely — this test
    // would have passed even without the capability grant because
    // `tool.execute(...)` was called directly via the side-table.
    //
    // The closure returns `Err(anyhow!(...))` on gate failure, so the
    // executor records `AsyncTaskStatus::Failed { error }` — the outer
    // `AsyncSpawnTool::execute` still returns Ok(receipt). We poll the
    // registry for the terminal status.
    #[tokio::test]
    async fn test_async_spawn_routes_through_capability_gate() {
        let core = Arc::new(ExtensionCore::new());
        core.insert_tool_instance("stub_tool".to_string(), Arc::new(StubTool))
            .await;

        let executor = Arc::new(AsyncExecutor::new());
        // No `tool:stub_tool` grant — gate must reject.
        let tool = AsyncSpawnTool::new(
            executor.clone(),
            Arc::downgrade(&core),
            None,
            PrincipalId::system().clone(),
            Arc::new(Vec::new()),
        );

        let result = tool
            .execute(json!({
                "tool": "stub_tool",
                "params": {"x": 1},
            }))
            .await
            .unwrap();
        let task_id = result["task_id"]
            .as_str()
            .expect("task_id present")
            .to_string();

        for _ in 0..50 {
            let entry_opt = {
                let reg = executor.registry().read().await;
                reg.get(&task_id).cloned()
            };
            if let Some(entry) = entry_opt {
                match &entry.status {
                    crate::extensions::framework::async_exec::executor::AsyncTaskStatus::Failed {
                        error,
                    } => {
                        assert!(
                            error.contains("stub_tool") && error.contains("disabled"),
                            "expected gate to reject stub_tool without capability, got: {error}"
                        );
                        return;
                    }
                    other => {
                        if matches!(
                            other,
                            crate::extensions::framework::async_exec::executor::AsyncTaskStatus::Pending
                                | crate::extensions::framework::async_exec::executor::AsyncTaskStatus::Running
                        ) {
                            // fall through to sleep
                        } else {
                            panic!(
                                "expected Failed status from gate, got terminal status: {other:?}"
                            );
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("spawned task {task_id} never recorded an outcome");
    }

    // F37: pin the allow path doesn't regress — with the right
    // capability, the gate passes and the spawned task runs.
    #[tokio::test]
    async fn test_async_spawn_routes_through_capability_gate_allow() {
        let core = Arc::new(ExtensionCore::new());
        core.insert_tool_instance("stub_tool".to_string(), Arc::new(StubTool))
            .await;

        let executor = Arc::new(AsyncExecutor::new());
        let tool = AsyncSpawnTool::new(
            executor.clone(),
            Arc::downgrade(&core),
            None,
            PrincipalId::system().clone(),
            Arc::new(vec!["tool:stub_tool".to_string()]),
        );

        let result = tool
            .execute(json!({
                "tool": "stub_tool",
                "params": {"x": 1},
            }))
            .await
            .unwrap();

        // Gate passed: we got a `task_id` (not a gate-error JSON).
        assert!(
            result.get("task_id").is_some(),
            "expected task_id, got {result:?}"
        );
        assert!(
            result.get("error").is_none(),
            "expected no error, got {result:?}"
        );
    }
}
