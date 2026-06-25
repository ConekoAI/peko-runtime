//! AsyncSpawn tool — invoke any tool asynchronously.
//!
//! Part of the Async* family that replaces the single `task` tool.
//! Requires an `AsyncExecutor` and an `ExtensionCore` reference.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::extensions::framework::async_exec::executor::{AsyncExecutor, AsyncToolConfig};
use crate::tools::core::Tool;

/// Spawn an async task invoking any registered tool.
pub struct AsyncSpawnTool {
    executor: Arc<AsyncExecutor>,
    extension_core: std::sync::Weak<crate::extensions::framework::core::ExtensionCore>,
    /// Agent identity (DID) used to look up this agent's session key on the
    /// shared `ExtensionCore` for `parent_session_key` stamping.
    agent_id: Option<String>,
}

impl AsyncSpawnTool {
    /// Construct with executor + extension core.
    ///
    /// `agent_id` is this tool's owning agent (typically the
    /// `Agent::identity.did`). It is used to look up the *correct* session key
    /// on the shared `ExtensionCore` so concurrent agents in daemon mode do not
    /// stamp each other's `parent_session_key` (issue #68).
    #[must_use]
    pub fn new(
        executor: Arc<AsyncExecutor>,
        extension_core: std::sync::Weak<crate::extensions::framework::core::ExtensionCore>,
        agent_id: Option<String>,
    ) -> Self {
        Self {
            executor,
            extension_core,
            agent_id,
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

        let task_id = format!("{}:{}", tool_name, uuid::Uuid::new_v4());

        // Look up the session key for *this* tool's owning agent (issue #68).
        let session_key = match self.agent_id.as_deref() {
            Some(agent_id) => core
                .current_session_key(agent_id)
                .unwrap_or_else(|| "unknown".to_string()),
            None => "unknown".to_string(),
        };

        let config = AsyncToolConfig {
            // `None` means no timeout: the spawned task runs to completion or
            // until cancelled via AsyncStop. The 5-min cap is applied by the
            // router on the *spawning* call, not on the spawned task's lifetime.
            timeout_secs: None,
            label,
            ..Default::default()
        };

        let tool = match core.get_tool(tool_name).await {
            Some(t) => t,
            None => {
                return Ok(json!({
                    "error": format!("tool '{}' not found", tool_name),
                    "tool_name": tool_name,
                }));
            }
        };

        let tool_params_for_closure = tool_params.clone();
        let receipt = self
            .executor
            .execute(
                task_id.clone(),
                tool_name,
                tool_params,
                session_key,
                config,
                move || async move { tool.execute(tool_params_for_closure).await },
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

    #[tokio::test]
    async fn test_async_spawn_missing_tool_returns_error() {
        let executor = Arc::new(AsyncExecutor::new());
        let core = Arc::new(crate::extensions::framework::core::ExtensionCore::new());
        let tool = AsyncSpawnTool::new(executor, Arc::downgrade(&core), None);

        let result = tool
            .execute(json!({"tool": "definitely_not_a_tool", "params": {}}))
            .await
            .unwrap();
        assert_eq!(result["error"], "tool 'definitely_not_a_tool' not found");
    }

    #[tokio::test]
    async fn test_async_spawn_stamps_correct_agent_session_key() {
        let core = Arc::new(crate::extensions::framework::core::ExtensionCore::new());
        core.insert_tool_instance("stub_tool".to_string(), Arc::new(StubTool))
            .await;

        let agent_a = "did:peko:agent:A";
        let agent_b = "did:peko:agent:B";
        core.set_session_key(agent_a, Some("sess-A".to_string()))
            .await;
        core.set_session_key(agent_b, Some("sess-B".to_string()))
            .await;

        let executor = Arc::new(AsyncExecutor::new());
        let tool_a = AsyncSpawnTool::new(
            executor.clone(),
            Arc::downgrade(&core),
            Some(agent_a.to_string()),
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
        let core = Arc::new(crate::extensions::framework::core::ExtensionCore::new());
        core.insert_tool_instance("stub_tool".to_string(), Arc::new(StubTool))
            .await;

        let executor = Arc::new(AsyncExecutor::new());
        let tool = AsyncSpawnTool::new(executor.clone(), Arc::downgrade(&core), None);

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
}
