//! `AsyncStopTool` — re-export shim.
//!
//! Phase 10c moved the implementation into
//! `peko_tools_builtin::async_control::stop`. Tests live in this shim
//! because the test fixture is framework-internal.

pub use peko_tools_builtin::async_control::AsyncStopTool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::Tool;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_async_stop_success() {
        let runtime = Arc::new(peko_extension_host::async_exec::executor::TestAsyncRuntime::new());
        runtime.insert(peko_extension_host::async_exec::executor::TestTaskEntry {
            task_id: "Bash:cancel-me".to_string(),
            tool_name: "Bash".to_string(),
            status: "pending".to_string(),
            parent_session_key: "session_1".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: None,
            result: None,
            label: None,
            metadata_type: "none".to_string(),
        });

        let tool = AsyncStopTool::new(runtime.as_shared());
        let result = tool
            .execute(json!({"task_id": "Bash:cancel-me"}))
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        assert_eq!(result["task_id"], "Bash:cancel-me");
        assert_eq!(result["previous_status"], "pending");
    }

    #[tokio::test]
    async fn test_async_stop_already_terminal() {
        let runtime = Arc::new(peko_extension_host::async_exec::executor::TestAsyncRuntime::new());
        runtime.insert(peko_extension_host::async_exec::executor::TestTaskEntry {
            task_id: "Bash:done".to_string(),
            tool_name: "Bash".to_string(),
            status: "completed".to_string(),
            parent_session_key: "session_1".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: None,
            result: None,
            label: None,
            metadata_type: "none".to_string(),
        });

        let tool = AsyncStopTool::new(runtime.as_shared());
        let result = tool.execute(json!({"task_id": "Bash:done"})).await.unwrap();

        // Already-terminal is a *successful* no-op, not an error —
        // the task didn't need cancelling. Callers should branch on
        // `already_terminal` rather than `success`.
        assert_eq!(result["success"], true);
        assert_eq!(result["already_terminal"], true);
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("already terminal"));
    }

    #[tokio::test]
    async fn test_async_stop_not_found() {
        let runtime = Arc::new(peko_extension_host::async_exec::executor::TestAsyncRuntime::new());
        let tool = AsyncStopTool::new(runtime.as_shared());
        let result = tool
            .execute(json!({"task_id": "Bash:missing"}))
            .await
            .unwrap();

        assert_eq!(result["success"], false);
        assert_eq!(result["message"], "Task not found");
    }
}
