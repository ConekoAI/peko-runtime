//! Integration tests for Agent Invoke Tool (GAP-005)
//!
//! These tests verify agent-to-agent messaging functionality.
//!
//! Run with: cargo test --test agent_invoke_integration -- --ignored

use pekobot::{
    agent::manager::AgentManager,
    tools::{
        AgentInvokeTool, InvocationMessage, InvocationResponse, InvocationService, InvokeCommand,
    },
};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Helper to create test agent manager
async fn setup_test_manager() -> (
    AgentManager,
    mpsc::Receiver<pekobot::agent::types::ManagerEvent>,
) {
    AgentManager::new()
        .await
        .expect("Failed to create agent manager")
}

/// Test that InvocationService can be created and started
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_invocation_service_creation() {
    let (service, command_tx) = InvocationService::new(None);

    // Start service in background
    let service_handle = tokio::spawn(async move {
        service.run().await;
    });

    // Give it a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Drop command channel to stop service
    drop(command_tx);

    // Wait for service to stop
    let result = tokio::time::timeout(tokio::time::Duration::from_secs(2), service_handle).await;

    assert!(result.is_ok(), "Service should stop gracefully");
}

/// Test async invocation through InvocationService
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_async_invocation_flow() {
    let (service, command_tx) = InvocationService::new(None);

    // Start service
    tokio::spawn(async move {
        service.run().await;
    });

    // Create an async invocation message
    let message = InvocationMessage {
        id: "test-async-123".to_string(),
        from: "did:peko:agent1".to_string(),
        to: "did:peko:agent2".to_string(),
        content: "Hello from test!".to_string(),
        context: serde_json::json!({}),
        timestamp: chrono::Utc::now(),
        reply_to: Some("test-async-123".to_string()),
        is_async: true,
        timeout_ms: 30000,
    };

    let (tx, mut rx) = mpsc::channel(1);

    // Send invocation
    command_tx
        .send(InvokeCommand::SendInvocation {
            message,
            respond_to: tx,
        })
        .await
        .expect("Failed to send command");

    // Receive result
    let result = tokio::time::timeout(tokio::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("Timeout waiting for result")
        .expect("Channel closed")
        .expect("Invocation failed");

    // Verify we got an accepted receipt
    use pekobot::tools::agent_invoke::InvocationResult;
    match result {
        InvocationResult::Accepted { receipt_id } => {
            assert_eq!(receipt_id, "test-async-123");
        }
        _ => panic!("Expected Accepted result, got {:?}", result),
    }

    drop(command_tx);
}

/// Test AgentInvokeTool creation and basic properties
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_agent_invoke_tool_creation() {
    use pekobot::tools::Tool;

    let (_service, command_tx) = InvocationService::new(None);

    let tool = AgentInvokeTool::new(
        "did:peko:test".to_string(),
        "test_agent".to_string(),
        command_tx,
        None,
    );

    // Verify tool properties
    assert_eq!(tool.name(), "agent_invoke");
    assert!(tool.description().contains("sync"));
    assert!(tool.description().contains("async"));

    // Verify parameters schema
    let schema = tool.parameters();
    assert_eq!(schema["type"].as_str().unwrap(), "object");
    assert!(schema["properties"]["target"].is_object());
    assert!(schema["properties"]["message"].is_object());
    assert!(schema["properties"]["mode"].is_object());
}

/// Test AgentManager creates agent_invoke tool in communication tools
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_manager_creates_invoke_tool() {
    let (manager, _events) = setup_test_manager().await;

    let tools = manager.create_communication_tools("did:peko:test");

    // Find agent_invoke tool
    let invoke_tool = tools.iter().find(|t| t.name() == "agent_invoke");
    assert!(
        invoke_tool.is_some(),
        "agent_invoke tool should be in communication tools"
    );

    // Verify other expected tools are present
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(tool_names.contains(&"agents_list"));
    assert!(tool_names.contains(&"agent_info"));
    assert!(tool_names.contains(&"agent_spawn"));
    assert!(tool_names.contains(&"agent_broadcast"));
}

/// Test that agent invocation ID is generated uniquely
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_invocation_id_uniqueness() {
    use pekobot::tools::Tool;

    let (service, command_tx) = InvocationService::new(None);
    tokio::spawn(async move {
        service.run().await;
    });

    let tool = AgentInvokeTool::new(
        "did:peko:test".to_string(),
        "test_agent".to_string(),
        command_tx.clone(),
        None,
    );

    // Execute two async invocations
    let params1 = serde_json::json!({
        "target": "agent2",
        "message": "Message 1",
        "mode": "async"
    });

    let params2 = serde_json::json!({
        "target": "agent2",
        "message": "Message 2",
        "mode": "async"
    });

    let result1 = tool
        .execute(params1)
        .await
        .expect("First invocation failed");
    let result2 = tool
        .execute(params2)
        .await
        .expect("Second invocation failed");

    let receipt_id1 = result1["receipt_id"].as_str().unwrap();
    let receipt_id2 = result2["receipt_id"].as_str().unwrap();

    // Verify IDs are different
    assert_ne!(receipt_id1, receipt_id2, "Receipt IDs should be unique");

    // Verify they look like UUIDs
    assert_eq!(receipt_id1.len(), 36, "Should be UUID format");
    assert_eq!(receipt_id2.len(), 36, "Should be UUID format");

    drop(command_tx);
}

/// Test timeout handling in sync mode (with mock handler)
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_sync_timeout_handling() {
    use async_trait::async_trait;
    use pekobot::tools::ExecuteHandler;
    use std::time::Duration;

    // Create a mock handler that always times out
    struct SlowHandler;

    #[async_trait]
    impl ExecuteHandler for SlowHandler {
        async fn execute_on_target(
            &self,
            _target: &str,
            _prompt: &str,
            _timeout_ms: u64,
        ) -> anyhow::Result<InvocationResponse> {
            // Simulate timeout by waiting longer than timeout
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(InvocationResponse {
                invocation_id: "test".to_string(),
                from: "target".to_string(),
                content: "Should not see this".to_string(),
                duration_ms: 10000,
                success: true,
                error: None,
            })
        }
    }

    let (mut service, command_tx) = InvocationService::new(None);
    service.set_execute_handler(Arc::new(SlowHandler));

    tokio::spawn(async move {
        service.run().await;
    });

    // Send execute command with short timeout
    let (tx, mut rx) = mpsc::channel(1);
    command_tx
        .send(InvokeCommand::ExecuteOnTarget {
            target: "slow_agent".to_string(),
            prompt: "test".to_string(),
            timeout_ms: 100, // Very short timeout
            respond_to: tx,
        })
        .await
        .expect("Failed to send command");

    // Wait for result
    let result = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("Test timeout")
        .expect("Channel closed")
        .expect("Handler error");

    // Verify we got a timeout response
    assert!(
        result.error.as_ref().unwrap().contains("Timeout"),
        "Expected timeout error, got: {:?}",
        result.error
    );
    assert!(!result.success);

    drop(command_tx);
}

/// Test invocation registry cleanup
#[tokio::test]
async fn test_invocation_registry_cleanup() {
    use pekobot::tools::agent_invoke::InvocationRegistry;
    use std::time::{Duration, Instant};

    let mut registry = InvocationRegistry::new();
    let (tx, _rx): (mpsc::Sender<InvocationResponse>, _) = mpsc::channel(1);

    // Register an old invocation
    let old_invocation = pekobot::tools::agent_invoke::PendingInvocation {
        id: "old".to_string(),
        target_did: "did:target".to_string(),
        source_did: "did:source".to_string(),
        created_at: Instant::now() - Duration::from_secs(600),
        response_tx: Some(tx),
    };

    registry.register(old_invocation);
    assert!(registry.get("old").is_some());

    // Clean up expired (older than 5 minutes)
    let expired = registry.cleanup_expired(Duration::from_secs(300));
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0], "old");
    assert!(registry.get("old").is_none());
}

/// Test error handling for missing parameters
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_agent_invoke_missing_params() {
    use pekobot::tools::Tool;

    let (_service, command_tx) = InvocationService::new(None);
    let tool = AgentInvokeTool::new(
        "did:peko:test".to_string(),
        "test_agent".to_string(),
        command_tx,
        None,
    );

    // Missing target
    let params = serde_json::json!({
        "message": "Hello"
    });
    let result = tool.execute(params).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("target"));

    // Missing message
    let params = serde_json::json!({
        "target": "agent2"
    });
    let result = tool.execute(params).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("message"));
}

/// Test complete flow: tool -> service -> handler -> response
#[tokio::test]
#[ignore = "Integration test - run manually with real agents"]
async fn test_complete_invocation_flow() {
    use async_trait::async_trait;
    use pekobot::tools::ExecuteHandler;
    use pekobot::tools::Tool;

    // Create a mock handler that simulates successful execution
    struct MockHandler;

    #[async_trait]
    impl ExecuteHandler for MockHandler {
        async fn execute_on_target(
            &self,
            target: &str,
            prompt: &str,
            _timeout_ms: u64,
        ) -> anyhow::Result<InvocationResponse> {
            // Simulate processing
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            Ok(InvocationResponse {
                invocation_id: "test-id".to_string(),
                from: target.to_string(),
                content: format!("Received prompt: {}", prompt),
                duration_ms: 100,
                success: true,
                error: None,
            })
        }
    }

    // Setup service with mock handler
    let (mut service, command_tx) = InvocationService::new(None);
    service.set_execute_handler(Arc::new(MockHandler));

    tokio::spawn(async move {
        service.run().await;
    });

    // Create tool
    let tool = AgentInvokeTool::new(
        "did:peko:agent1".to_string(),
        "agent1".to_string(),
        command_tx.clone(),
        None,
    );

    // Execute sync invocation
    let params = serde_json::json!({
        "target": "agent2",
        "message": "What is your name?",
        "mode": "sync",
        "timeout_ms": 5000
    });

    let result = tool.execute(params).await.expect("Invocation failed");

    // Verify result structure
    assert!(result["success"].as_bool().unwrap());
    assert_eq!(result["status"].as_str().unwrap(), "completed");
    assert!(result["result"].as_str().is_some());
    assert!(result["duration_ms"].as_u64().is_some());
    assert_eq!(result["from"].as_str().unwrap(), "agent2");

    drop(command_tx);
}
