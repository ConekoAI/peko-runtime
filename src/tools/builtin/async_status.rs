//! `AsyncStatusTool` — re-export shim.
//!
//! Phase 10c moved the implementation into
//! `peko_tools_builtin::async_control::status`. Tests live in this
//! shim because the test fixture is framework-internal.

pub use peko_tools_builtin::async_control::AsyncStatusTool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::Tool;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_async_status_not_found() {
        let runtime = Arc::new(peko_extension_host::async_exec::executor::TestAsyncRuntime::new());
        let tool = AsyncStatusTool::new(runtime.as_shared());
        let result = tool
            .execute(json!({"task_id": "nonexistent:task"}))
            .await
            .unwrap();
        assert_eq!(result["error"], "Task not found");
        assert_eq!(result["task_id"], "nonexistent:task");
    }

    #[tokio::test]
    async fn test_async_status_with_runtime() {
        let runtime = Arc::new(peko_extension_host::async_exec::executor::TestAsyncRuntime::new());
        runtime.insert(peko_extension_host::async_exec::executor::TestTaskEntry {
            task_id: "Bash:test-123".to_string(),
            tool_name: "Bash".to_string(),
            status: "pending".to_string(),
            parent_session_key: "session_1".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: None,
            result: None,
            label: None,
            metadata_type: "none".to_string(),
        });

        let tool = AsyncStatusTool::new(runtime.as_shared());
        let result = tool
            .execute(json!({"task_id": "Bash:test-123"}))
            .await
            .unwrap();

        assert_eq!(result["task_id"], "Bash:test-123");
        assert_eq!(result["tool_name"], "Bash");
        assert_eq!(result["status"], "pending");
        assert_eq!(result["metadata_type"], "none");
    }
}
