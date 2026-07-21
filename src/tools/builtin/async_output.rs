//! `AsyncOutputTool` — re-export shim.
//!
//! Phase 10c moved the implementation into
//! `peko_tools_builtin::async_control::output`. Tests live in this shim
//! because the test fixture is framework-internal.

pub use peko_tools_builtin::async_control::AsyncOutputTool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::Tool;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_async_output_tail_lines_string_result() {
        let runtime =
            Arc::new(crate::extensions::framework::async_exec::executor::TestAsyncRuntime::new());
        let result_value = json!("line1\nline2\nline3\nline4\nline5");
        runtime.insert(
            crate::extensions::framework::async_exec::executor::TestTaskEntry {
                task_id: "Bash:string-result".to_string(),
                tool_name: "Bash".to_string(),
                status: "completed".to_string(),
                parent_session_key: "session_1".to_string(),
                created_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                result: Some(result_value),
                label: None,
                metadata_type: "none".to_string(),
            },
        );

        let tool = AsyncOutputTool::new(runtime.as_shared());
        let result = tool
            .execute(json!({
                "task_id": "Bash:string-result",
                "tail_lines": 2
            }))
            .await
            .unwrap();

        assert_eq!(result["result"], "line4\nline5");
    }

    #[tokio::test]
    async fn test_async_output_tail_lines_object_with_stdout() {
        let runtime =
            Arc::new(crate::extensions::framework::async_exec::executor::TestAsyncRuntime::new());
        let result_value = json!({
            "stdout": "line1\nline2\nline3",
            "exit_code": 0
        });
        runtime.insert(
            crate::extensions::framework::async_exec::executor::TestTaskEntry {
                task_id: "Bash:obj-result".to_string(),
                tool_name: "Bash".to_string(),
                status: "completed".to_string(),
                parent_session_key: "session_1".to_string(),
                created_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                result: Some(result_value),
                label: None,
                metadata_type: "none".to_string(),
            },
        );

        let tool = AsyncOutputTool::new(runtime.as_shared());
        let result = tool
            .execute(json!({
                "task_id": "Bash:obj-result",
                "tail_lines": 2
            }))
            .await
            .unwrap();

        assert_eq!(result["result"]["stdout"], "line2\nline3");
        assert_eq!(result["result"]["exit_code"], 0);
    }

    #[tokio::test]
    async fn test_async_output_tail_lines_unknown_shape_passthrough() {
        let runtime =
            Arc::new(crate::extensions::framework::async_exec::executor::TestAsyncRuntime::new());
        let result_value = json!({"count": 42});
        runtime.insert(
            crate::extensions::framework::async_exec::executor::TestTaskEntry {
                task_id: "Bash:unknown-shape".to_string(),
                tool_name: "Bash".to_string(),
                status: "completed".to_string(),
                parent_session_key: "session_1".to_string(),
                created_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                result: Some(result_value),
                label: None,
                metadata_type: "none".to_string(),
            },
        );

        let tool = AsyncOutputTool::new(runtime.as_shared());
        let result = tool
            .execute(json!({
                "task_id": "Bash:unknown-shape",
                "tail_lines": 10
            }))
            .await
            .unwrap();

        assert_eq!(result["result"], json!({"count": 42}));
    }

    #[tokio::test]
    async fn test_async_output_tail_lines_zero_passthrough() {
        let runtime =
            Arc::new(crate::extensions::framework::async_exec::executor::TestAsyncRuntime::new());
        let result_value = json!("line1\nline2\nline3\nline4\nline5");
        runtime.insert(
            crate::extensions::framework::async_exec::executor::TestTaskEntry {
                task_id: "Bash:zero".to_string(),
                tool_name: "Bash".to_string(),
                status: "completed".to_string(),
                parent_session_key: "session_1".to_string(),
                created_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                result: Some(result_value),
                label: None,
                metadata_type: "none".to_string(),
            },
        );

        let tool = AsyncOutputTool::new(runtime.as_shared());
        let result = tool.execute(json!({"task_id": "Bash:zero"})).await.unwrap();

        assert_eq!(result["result"], "line1\nline2\nline3\nline4\nline5");
    }
}
