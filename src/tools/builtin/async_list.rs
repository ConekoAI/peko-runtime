//! `AsyncListTool` — re-export shim.
//!
//! Phase 10c moved the implementation into
//! `peko_tools_builtin::async_control::list`. The legacy
//! `global()` cross-registry mode is gone — the tool now holds an
//! `Arc<dyn AsyncRuntime>` and operates on the runtime's per-agent
//! scope. Tests live in this shim because the test fixture
//! (`AsyncTaskRegistry`) is framework-internal.

pub use peko_tools_builtin::async_control::AsyncListTool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::async_exec::executor::{AsyncTaskEntry, AsyncTaskStatus};
    use crate::tools::ToolResult;
    use peko_tools_core::traits::Tool;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_async_list_empty() {
        let runtime =
            Arc::new(crate::extensions::framework::async_exec::executor::TestAsyncRuntime::new());
        let tool = AsyncListTool::new(runtime.as_shared());
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 0);
        assert_eq!(result["active"], 0);
    }

    #[tokio::test]
    async fn test_async_list_with_filters() {
        let runtime =
            Arc::new(crate::extensions::framework::async_exec::executor::TestAsyncRuntime::new());
        runtime.insert(
            crate::extensions::framework::async_exec::executor::TestTaskEntry {
                task_id: "Bash:test-1".to_string(),
                tool_name: "Bash".to_string(),
                status: "completed".to_string(),
                parent_session_key: "session_1".to_string(),
                created_at: chrono::Utc::now(),
                completed_at: None,
                result: Some(json!({"done": true})),
                label: None,
                metadata_type: "none".to_string(),
            },
        );
        runtime.insert(
            crate::extensions::framework::async_exec::executor::TestTaskEntry {
                task_id: "Agent:test-2".to_string(),
                tool_name: "Agent".to_string(),
                status: "pending".to_string(),
                parent_session_key: "session_1".to_string(),
                created_at: chrono::Utc::now(),
                completed_at: None,
                result: None,
                label: None,
                metadata_type: "none".to_string(),
            },
        );

        let tool = AsyncListTool::new(runtime.as_shared());

        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["active"], 1);

        let result = tool.execute(json!({"tool_filter": "Bash"})).await.unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["tool_name"], "Bash");

        let result = tool
            .execute(json!({"status_filter": "completed"}))
            .await
            .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["status"], "completed");
    }

    // Suppress unused-import warnings by referencing the unused names.
    #[allow(dead_code)]
    fn _pin_unused(_: AsyncTaskEntry, _: AsyncTaskStatus, _: ToolResult) {}
}
