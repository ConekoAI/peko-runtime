//! Tool Executor Tests
//!
//! Tests for unified tool execution with context injection.
//! This ensures all tool types (built-in, universal, MCP) receive proper context.

#[cfg(test)]
mod tests {
    use crate::engine::tool_executor::{ToolExecutionContext, ToolExecutor};
    use crate::tools::{Tool, ToolContext};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    /// Mock tool that records whether it received context
    struct ContextAwareMockTool {
        name: String,
        received_context: std::sync::Mutex<Option<ToolContext>>,
    }

    impl ContextAwareMockTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                received_context: std::sync::Mutex::new(None),
            }
        }

        fn was_called_with_context(&self) -> bool {
            self.received_context.lock().unwrap().is_some()
        }

        fn get_received_context(&self) -> Option<ToolContext> {
            self.received_context.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Tool for ContextAwareMockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> String {
            "Mock tool for testing context injection".to_string()
        }

        fn parameters(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                }
            })
        }

        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            // This should NOT be called when execute_with_context is available
            Ok(json!({"result": "called_without_context"}))
        }

        async fn execute_with_context(
            &self,
            params: serde_json::Value,
            ctx: &ToolContext,
        ) -> anyhow::Result<serde_json::Value> {
            // Record that we received context
            *self.received_context.lock().unwrap() = Some(ctx.clone());

            // Return both params and context info for verification
            Ok(json!({
                "result": "success",
                "input": params.get("input"),
                "agent_id": ctx.agent_id,
                "session_id": ctx.session_id,
                "run_id": ctx.run_id,
            }))
        }
    }

    #[tokio::test]
    async fn test_tool_executor_calls_execute_with_context() {
        let tool = Arc::new(ContextAwareMockTool::new("test_tool"));
        let executor = ToolExecutor::new();

        let exec_context = ToolExecutionContext {
            agent_id: "agent_123".to_string(),
            session_id: "session_456".to_string(),
            run_id: "run_789".to_string(),
            peer_id: None,
            workspace: "/tmp/test".to_string(),
        };

        let params = json!({"input": "hello"});

        // Clone for use after execution
        let tool_clone = Arc::clone(&tool);

        let result = executor
            .execute_with_context(tool, params, &exec_context)
            .await;

        // Should succeed
        assert!(result.is_ok());

        // Should have called execute_with_context
        assert!(
            tool_clone.was_called_with_context(),
            "Tool should have received context"
        );

        // Verify context was passed correctly
        let ctx = tool_clone.get_received_context().unwrap();
        assert_eq!(ctx.agent_id, Some("agent_123".to_string()));
        assert_eq!(ctx.session_id, Some("session_456".to_string()));
        assert_eq!(ctx.run_id, "run_789");
    }

    #[tokio::test]
    async fn test_tool_executor_injects_context_into_result() {
        let tool = Arc::new(ContextAwareMockTool::new("identity_tool"));
        let executor = ToolExecutor::new();

        let exec_context = ToolExecutionContext {
            agent_id: "test_agent".to_string(),
            session_id: "test_session".to_string(),
            run_id: "test_run".to_string(),
            peer_id: Some("peer_123".to_string()),
            workspace: "/workspace".to_string(),
        };

        let params = json!({"input": "test"});

        let result = executor
            .execute_with_context(tool, params, &exec_context)
            .await
            .unwrap();

        // Verify the result contains context info
        assert_eq!(result["agent_id"], "test_agent");
        assert_eq!(result["session_id"], "test_session");
        assert_eq!(result["run_id"], "test_run");
        assert_eq!(result["result"], "success");
    }

    #[tokio::test]
    async fn test_tool_executor_preserves_params() {
        let tool = Arc::new(ContextAwareMockTool::new("params_tool"));
        let executor = ToolExecutor::new();

        let exec_context = ToolExecutionContext {
            agent_id: "agent".to_string(),
            session_id: "session".to_string(),
            run_id: "run".to_string(),
            peer_id: None,
            workspace: "/tmp".to_string(),
        };

        let params = json!({
            "input": "test_value",
            "number": 42,
            "nested": {"key": "value"}
        });

        let result = executor
            .execute_with_context(tool, params.clone(), &exec_context)
            .await
            .unwrap();

        // Original params should be preserved
        assert_eq!(result["input"], "test_value");
    }
}
