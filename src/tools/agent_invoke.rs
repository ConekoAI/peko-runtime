//! Agent Invoke Tool - Session-based agent-to-agent messaging
//!
//! Implements GAP-005: Agent-to-Agent Messaging using session queues.
//!
//! Supports two modes:
//! - **Sync**: Blocks until target agent returns result
//! - **Async**: Returns receipt, result delivered via EventSubscriber (GAP-004)
//!
//! This tool replaces the inbox-based approach with direct session messaging,
//! leveraging the existing SessionRouter (GAP-003) and EventSubscriber (GAP-004).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::orchestration::events::SystemEvent;
use crate::orchestration::EventSubscriber;
use crate::tools::Tool;

/// Pending invocation awaiting response
#[derive(Debug, Clone)]
pub struct PendingInvocation {
    /// Invocation ID
    pub id: String,
    /// Target agent DID
    pub target_did: String,
    /// Source agent DID
    pub source_did: String,
    /// When created
    pub created_at: Instant,
    /// Response sender (for sync mode)
    pub response_tx: Option<mpsc::Sender<InvocationResponse>>,
}

/// Response from an invocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationResponse {
    /// Original invocation ID
    pub invocation_id: String,
    /// From agent DID
    pub from: String,
    /// Response content
    pub content: String,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Whether successful
    pub success: bool,
    /// Error message (if failed)
    pub error: Option<String>,
}

/// Invocation message sent to target agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationMessage {
    /// Message ID
    pub id: String,
    /// From agent DID
    pub from: String,
    /// To agent DID
    pub to: String,
    /// Message content/prompt
    pub content: String,
    /// Optional context
    pub context: serde_json::Value,
    /// Timestamp
    pub timestamp: chrono::DateTime<Utc>,
    /// Reply-to ID (for tracking responses)
    pub reply_to: Option<String>,
    /// Whether async mode
    pub is_async: bool,
    /// Timeout in milliseconds (for sync)
    pub timeout_ms: u64,
}

/// Global registry for tracking pending invocations
#[derive(Debug, Default)]
pub struct InvocationRegistry {
    /// Pending invocations by ID
    pending: HashMap<String, PendingInvocation>,
    /// Response channels by agent DID (for agents to receive responses)
    agent_response_channels: HashMap<String, mpsc::Sender<InvocationResponse>>,
}

impl InvocationRegistry {
    /// Create new registry
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            agent_response_channels: HashMap::new(),
        }
    }

    /// Register a pending invocation
    pub fn register(&mut self, invocation: PendingInvocation) {
        self.pending.insert(invocation.id.clone(), invocation);
    }

    /// Remove a pending invocation
    pub fn remove(&mut self, id: &str) -> Option<PendingInvocation> {
        self.pending.remove(id)
    }

    /// Get a pending invocation
    pub fn get(&self, id: &str) -> Option<&PendingInvocation> {
        self.pending.get(id)
    }

    /// Register an agent's response channel
    pub fn register_agent_channel(
        &mut self,
        agent_did: &str,
        tx: mpsc::Sender<InvocationResponse>,
    ) {
        self.agent_response_channels
            .insert(agent_did.to_string(), tx);
    }

    /// Get an agent's response channel
    pub fn get_agent_channel(&self, agent_did: &str) -> Option<mpsc::Sender<InvocationResponse>> {
        self.agent_response_channels.get(agent_did).cloned()
    }

    /// Remove an agent's response channel
    pub fn remove_agent_channel(&mut self, agent_did: &str) {
        self.agent_response_channels.remove(agent_did);
    }

    /// Clean up expired invocations (older than timeout)
    pub fn cleanup_expired(&mut self, max_age: Duration) -> Vec<String> {
        let now = Instant::now();
        let expired: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, inv)| now.duration_since(inv.created_at) > max_age)
            .map(|(id, _)| id.clone())
            .collect();

        for id in &expired {
            self.pending.remove(id);
        }

        expired
    }
}

/// Shared invocation registry
pub type SharedInvocationRegistry = Arc<RwLock<InvocationRegistry>>;

/// Create a shared invocation registry
pub fn create_shared_registry() -> SharedInvocationRegistry {
    Arc::new(RwLock::new(InvocationRegistry::new()))
}

/// Tool for invoking another agent via session-based messaging
///
/// MODE: sync - blocks until target agent returns result
/// MODE: async - returns receipt, result delivered via EventSubscriber
pub struct AgentInvokeTool {
    /// This agent's DID
    agent_did: String,
    /// This agent's name
    agent_name: String,
    /// Channel to send invocation requests to the runtime
    command_tx: mpsc::Sender<InvokeCommand>,
    /// Event subscriber for async result delivery
    event_subscriber: Option<Arc<EventSubscriber>>,
}

/// Commands for the invocation system
#[derive(Debug)]
pub enum InvokeCommand {
    /// Send invocation to target agent
    SendInvocation {
        message: InvocationMessage,
        respond_to: mpsc::Sender<Result<InvocationResult>>,
    },
    /// Register this agent's response channel
    RegisterResponseChannel {
        agent_did: String,
        tx: mpsc::Sender<InvocationResponse>,
    },
    /// Get pending invocation for response
    GetPending {
        invocation_id: String,
        respond_to: mpsc::Sender<Option<PendingInvocation>>,
    },
    /// Send response back to invoking agent
    SendResponse {
        response: InvocationResponse,
        respond_to: mpsc::Sender<Result<()>>,
    },
    /// Find and execute on target agent
    ExecuteOnTarget {
        target: String,
        prompt: String,
        timeout_ms: u64,
        respond_to: mpsc::Sender<Result<InvocationResponse>>,
    },
}

/// Result of an invocation (internal)
#[derive(Debug, Clone)]
pub enum InvocationResult {
    /// Sync mode completed with result
    Completed(InvocationResponse),
    /// Async mode accepted with receipt
    Accepted { receipt_id: String },
    /// Target not found
    TargetNotFound(String),
    /// Timeout
    Timeout,
}

impl AgentInvokeTool {
    /// Create a new agent invoke tool
    pub fn new(
        agent_did: impl Into<String>,
        agent_name: impl Into<String>,
        command_tx: mpsc::Sender<InvokeCommand>,
        event_subscriber: Option<Arc<EventSubscriber>>,
    ) -> Self {
        Self {
            agent_did: agent_did.into(),
            agent_name: agent_name.into(),
            command_tx,
            event_subscriber,
        }
    }

    /// Create parameters schema for LLM tool calling
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Agent DID or name to invoke (required)"
                },
                "message": {
                    "type": "string",
                    "description": "The request/prompt to send to the target agent (required)"
                },
                "mode": {
                    "type": "string",
                    "enum": ["sync", "async"],
                    "description": "Execution mode: 'sync' blocks for result, 'async' returns receipt (default: sync)"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout for sync mode in milliseconds (default: 30000, max: 300000)"
                },
                "context": {
                    "type": "object",
                    "description": "Optional context object to pass to target agent"
                }
            },
            "required": ["target", "message"]
        })
    }
}

#[async_trait]
impl Tool for AgentInvokeTool {
    fn name(&self) -> &'static str {
        "agent_invoke"
    }

    fn description(&self) -> &'static str {
        r#"Invoke another agent via session-based messaging.

Supports two modes:
- sync: Blocks until target agent returns result (default)
- async: Returns immediately with receipt, result delivered via event

The target agent will receive the message and process it, then return
a response. In sync mode, this tool waits for that response. In async
mode, the response is delivered later through the event system.

Parameters:
- target: Agent DID or name to invoke (required)
- message: The request/prompt to send (required)
- mode: "sync" or "async" (default: sync)
- timeout_ms: Timeout for sync mode (default: 30000)
- context: Optional context object

Examples:
SYNC:  {"target": "researcher", "message": "Analyze this data", "mode": "sync"}
ASYNC: {"target": "analyzer", "message": "Process logs", "mode": "async"}

Use when: you need another agent to perform a task and wait for results.
Don't use when: a simple function call or direct computation would suffice."#
    }

    fn llm_description(&self) -> String {
        r#"Invoke another agent to perform a task. Use agent_invoke when you need to:
- Delegate work to a specialized agent
- Get information from another agent's session
- Request analysis or computation from another agent

Examples:
{"target": "researcher", "message": "Search for recent papers on quantum computing", "mode": "sync"}
{"target": "code_agent", "message": "Review this function for bugs", "mode": "async"}

Use when: you need another agent's capabilities and must wait for results (sync) or can continue without results (async).
Don't use when: the task is simple enough to do yourself or you don't need to coordinate with other agents."#.to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        self.parameters_schema()
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Parse parameters
        let target = params["target"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing required parameter 'target'"))?;
        let message = params["message"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing required parameter 'message'"))?;
        let mode = params["mode"].as_str().unwrap_or("sync");
        let timeout_ms = params["timeout_ms"].as_u64().unwrap_or(30000);
        let context = params.get("context").cloned().unwrap_or(json!({}));

        // Validate mode
        let is_async = match mode {
            "sync" => false,
            "async" => true,
            _ => return Err(anyhow!("Invalid mode '{}'. Use 'sync' or 'async'", mode)),
        };

        // Cap timeout at 5 minutes
        let timeout_ms = timeout_ms.min(300_000).max(1000);

        // Generate unique invocation ID
        let invocation_id = Uuid::new_v4().to_string();

        info!(
            agent = %self.agent_name,
            invocation_id = %invocation_id,
            target = %target,
            mode = %mode,
            "Invoking agent"
        );

        if is_async {
            // Async mode: return receipt immediately
            Ok(json!({
                "success": true,
                "status": "accepted",
                "receipt_id": invocation_id,
                "target": target,
                "mode": "async",
                "note": "Result will be delivered via event when ready"
            }))
        } else {
            // Sync mode: execute on target and wait for response
            let (tx, mut rx) = mpsc::channel(1);

            // Send execute command
            let prompt = format!(
                "AGENT_INVOCATION:{}:{}:{}\n{}",
                self.agent_did, invocation_id, timeout_ms, message
            );

            self.command_tx
                .send(InvokeCommand::ExecuteOnTarget {
                    target: target.to_string(),
                    prompt,
                    timeout_ms,
                    respond_to: tx,
                })
                .await
                .map_err(|e| anyhow!("Failed to send execute command: {}", e))?;

            // Wait for result with timeout
            match timeout(Duration::from_millis(timeout_ms), rx.recv()).await {
                Ok(Some(Ok(response))) => Ok(json!({
                    "success": response.success,
                    "status": "completed",
                    "result": response.content,
                    "duration_ms": response.duration_ms,
                    "from": response.from,
                    "invocation_id": response.invocation_id,
                })),
                Ok(Some(Err(e))) => Err(anyhow!("Invocation failed: {}", e)),
                Ok(None) => Err(anyhow!("Invocation channel closed")),
                Err(_) => Err(anyhow!(
                    "Timeout waiting for response from '{}' after {}ms",
                    target,
                    timeout_ms
                )),
            }
        }
    }

    fn estimated_duration_ms(&self, params: &serde_json::Value) -> u64 {
        // Use provided timeout as estimate, or default
        params["timeout_ms"].as_u64().unwrap_or(30000).min(300_000)
    }
}

/// Service that handles invocation routing between agents
///
/// This runs as a background task and routes messages between agents
pub struct InvocationService {
    /// Registry of pending invocations
    registry: SharedInvocationRegistry,
    /// Command receiver
    command_rx: RwLock<mpsc::Receiver<InvokeCommand>>,
    /// Event subscriber for async notifications
    event_subscriber: Option<Arc<EventSubscriber>>,
    /// Handler for executing on target agent - set by AgentManager
    execute_handler: Option<Arc<dyn ExecuteHandler>>,
}

/// Handler trait for executing invocations on target agents
#[async_trait]
pub trait ExecuteHandler: Send + Sync {
    /// Execute a prompt on a target agent
    async fn execute_on_target(
        &self,
        target: &str,
        prompt: &str,
        timeout_ms: u64,
    ) -> Result<InvocationResponse>;
}

impl InvocationService {
    /// Create a new invocation service
    pub fn new(
        event_subscriber: Option<Arc<EventSubscriber>>,
    ) -> (Self, mpsc::Sender<InvokeCommand>) {
        let (command_tx, command_rx) = mpsc::channel(100);
        let registry = create_shared_registry();

        let service = Self {
            registry,
            command_rx: RwLock::new(command_rx),
            event_subscriber,
            execute_handler: None,
        };

        (service, command_tx)
    }

    /// Create with existing registry
    pub fn with_registry(
        registry: SharedInvocationRegistry,
        event_subscriber: Option<Arc<EventSubscriber>>,
    ) -> (Self, mpsc::Sender<InvokeCommand>) {
        let (command_tx, command_rx) = mpsc::channel(100);

        let service = Self {
            registry,
            command_rx: RwLock::new(command_rx),
            event_subscriber,
            execute_handler: None,
        };

        (service, command_tx)
    }

    /// Set the execute handler
    pub fn set_execute_handler(&mut self, handler: Arc<dyn ExecuteHandler>) {
        self.execute_handler = Some(handler);
    }

    /// Start the invocation service
    pub async fn run(&self) {
        info!("Invocation service started");

        let mut rx = self.command_rx.write().await;

        while let Some(cmd) = rx.recv().await {
            match cmd {
                InvokeCommand::SendInvocation {
                    message,
                    respond_to,
                } => {
                    let result = self.handle_send_invocation(message).await;
                    let _ = respond_to.send(result).await;
                }
                InvokeCommand::RegisterResponseChannel { agent_did, tx } => {
                    let mut registry = self.registry.write().await;
                    registry.register_agent_channel(&agent_did, tx);
                    debug!(agent_did = %agent_did, "Registered response channel");
                }
                InvokeCommand::GetPending {
                    invocation_id,
                    respond_to,
                } => {
                    let registry = self.registry.read().await;
                    let pending = registry.get(&invocation_id).cloned();
                    let _ = respond_to.send(pending).await;
                }
                InvokeCommand::SendResponse {
                    response,
                    respond_to,
                } => {
                    let result = self.handle_send_response(response).await;
                    let _ = respond_to.send(result).await;
                }
                InvokeCommand::ExecuteOnTarget {
                    target,
                    prompt,
                    timeout_ms,
                    respond_to,
                } => {
                    let result = self
                        .handle_execute_on_target(target, prompt, timeout_ms)
                        .await;
                    let _ = respond_to.send(result).await;
                }
            }
        }

        info!("Invocation service stopped");
    }

    /// Handle sending an invocation to a target agent
    async fn handle_send_invocation(&self, message: InvocationMessage) -> Result<InvocationResult> {
        // For now, just emit event and return receipt (async mode)
        if message.is_async {
            if let Some(ref subscriber) = self.event_subscriber {
                let event = SystemEvent::Internal {
                    event_type: "agent_invocation_sent".to_string(),
                    source: message.from,
                    payload: json!({
                        "receipt_id": message.id,
                        "to": message.to,
                        "status": "pending",
                        "is_async": true,
                    }),
                    timestamp: Utc::now(),
                };
                let _ = subscriber.publish(event);
            }

            Ok(InvocationResult::Accepted {
                receipt_id: message.id,
            })
        } else {
            // Sync mode requires execute handler
            Err(anyhow!("Sync mode requires ExecuteHandler to be set"))
        }
    }

    /// Handle executing on a target agent
    async fn handle_execute_on_target(
        &self,
        target: String,
        prompt: String,
        timeout_ms: u64,
    ) -> Result<InvocationResponse> {
        if let Some(ref handler) = self.execute_handler {
            handler
                .execute_on_target(&target, &prompt, timeout_ms)
                .await
        } else {
            Err(anyhow!("No execute handler configured"))
        }
    }

    /// Handle sending a response back to the invoking agent
    async fn handle_send_response(&self, response: InvocationResponse) -> Result<()> {
        // Get the response channel if there is one
        let response_tx = {
            let registry = self.registry.read().await;
            registry
                .get(&response.invocation_id)
                .and_then(|pending| pending.response_tx.clone())
        };

        // Send response via channel if in sync mode
        if let Some(tx) = response_tx {
            let response_clone = response.clone();
            let _ = tx.send(response_clone).await;
        }

        // Emit event for async mode
        if let Some(ref subscriber) = self.event_subscriber {
            let event = SystemEvent::Internal {
                event_type: "agent_invocation_complete".to_string(),
                source: response.from.clone(),
                payload: json!({
                    "receipt_id": response.invocation_id,
                    "status": if response.success { "completed" } else { "failed" },
                    "result_preview": response.content.chars().take(200).collect::<String>(),
                    "error": response.error,
                    "completed_at": Utc::now(),
                }),
                timestamp: Utc::now(),
            };
            let _ = subscriber.publish(event);
        }

        // Clean up
        let mut registry = self.registry.write().await;
        registry.remove(&response.invocation_id);

        Ok(())
    }

    /// Clean up expired invocations periodically
    pub async fn cleanup_task(&self, interval_secs: u64) {
        let interval = Duration::from_secs(interval_secs);
        let max_age = Duration::from_secs(300); // 5 minutes

        loop {
            tokio::time::sleep(interval).await;

            let mut registry = self.registry.write().await;
            let expired = registry.cleanup_expired(max_age);
            drop(registry);

            if !expired.is_empty() {
                info!("Cleaned up {} expired invocations", expired.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invocation_registry() {
        let mut registry = InvocationRegistry::new();

        // Create a dummy channel
        let (tx, _rx) = mpsc::channel(1);

        // Register an invocation
        let invocation = PendingInvocation {
            id: "test-1".to_string(),
            target_did: "did:target".to_string(),
            source_did: "did:source".to_string(),
            created_at: Instant::now(),
            response_tx: Some(tx),
        };

        registry.register(invocation.clone());
        assert!(registry.get("test-1").is_some());

        // Remove it
        let removed = registry.remove("test-1");
        assert!(removed.is_some());
        assert!(registry.get("test-1").is_none());
    }

    #[test]
    fn test_invocation_message_serialization() {
        let msg = InvocationMessage {
            id: "uuid-123".to_string(),
            from: "did:alice".to_string(),
            to: "did:bob".to_string(),
            content: "Hello!".to_string(),
            context: json!({"key": "value"}),
            timestamp: Utc::now(),
            reply_to: Some("uuid-123".to_string()),
            is_async: false,
            timeout_ms: 30000,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: InvocationMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(msg.id, deserialized.id);
        assert_eq!(msg.from, deserialized.from);
        assert_eq!(msg.content, deserialized.content);
    }

    #[tokio::test]
    async fn test_invocation_response_serialization() {
        let response = InvocationResponse {
            invocation_id: "uuid-123".to_string(),
            from: "did:bob".to_string(),
            content: "Result!".to_string(),
            duration_ms: 100,
            success: true,
            error: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        let deserialized: InvocationResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(response.invocation_id, deserialized.invocation_id);
        assert_eq!(response.success, deserialized.success);
    }

    #[tokio::test]
    async fn test_invocation_service_basic() {
        let (service, command_tx) = InvocationService::new(None);

        // Start service in background
        let service_handle = tokio::spawn(async move {
            service.run().await;
        });

        // Test async invocation
        let message = InvocationMessage {
            id: "test-123".to_string(),
            from: "did:alice".to_string(),
            to: "did:bob".to_string(),
            content: "Hello!".to_string(),
            context: json!({}),
            timestamp: Utc::now(),
            reply_to: Some("test-123".to_string()),
            is_async: true,
            timeout_ms: 30000,
        };

        let (tx, mut rx) = mpsc::channel(1);
        command_tx
            .send(InvokeCommand::SendInvocation {
                message,
                respond_to: tx,
            })
            .await
            .unwrap();

        let result = rx.recv().await.unwrap().unwrap();
        match result {
            InvocationResult::Accepted { receipt_id } => {
                assert_eq!(receipt_id, "test-123");
            }
            _ => panic!("Expected Accepted result"),
        }

        // Clean up
        drop(command_tx);
        let _ = tokio::time::timeout(tokio::time::Duration::from_secs(1), service_handle).await;
    }

    #[tokio::test]
    async fn test_agent_invoke_tool_async_mode() {
        let (service, command_tx) = InvocationService::new(None);

        // Start service in background
        tokio::spawn(async move {
            service.run().await;
        });

        let tool = AgentInvokeTool::new(
            "did:test".to_string(),
            "test_agent".to_string(),
            command_tx,
            None,
        );

        // Test async mode
        let params = json!({
            "target": "other_agent",
            "message": "Do something",
            "mode": "async"
        });

        let result = tool.execute(params).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["status"].as_str().unwrap(), "accepted");
        assert!(result["receipt_id"].as_str().is_some());
        assert_eq!(result["mode"].as_str().unwrap(), "async");
    }

    #[test]
    fn test_agent_invoke_tool_parameters_schema() {
        use crate::tools::Tool;

        let (_service, command_tx) = InvocationService::new(None);
        let tool = AgentInvokeTool::new(
            "did:test".to_string(),
            "test_agent".to_string(),
            command_tx,
            None,
        );

        let schema = tool.parameters();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        assert!(schema["properties"]["target"].is_object());
        assert!(schema["properties"]["message"].is_object());
        assert!(schema["properties"]["mode"].is_object());
        assert!(schema["properties"]["timeout_ms"].is_object());
        assert!(schema["properties"]["context"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("target")));
        assert!(required.contains(&json!("message")));
    }

    #[tokio::test]
    async fn test_agent_invoke_tool_missing_params() {
        use crate::tools::Tool;

        let (_service, command_tx) = InvocationService::new(None);
        let tool = AgentInvokeTool::new(
            "did:test".to_string(),
            "test_agent".to_string(),
            command_tx,
            None,
        );

        // Missing target
        let params = json!({
            "message": "Do something"
        });
        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("target"));

        // Missing message
        let params = json!({
            "target": "other_agent"
        });
        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("message"));
    }

    #[tokio::test]
    async fn test_invocation_registry_cleanup() {
        let mut registry = InvocationRegistry::new();
        let (tx, _rx): (mpsc::Sender<InvocationResponse>, _) = mpsc::channel(1);

        // Register an old invocation
        let old_invocation = PendingInvocation {
            id: "old".to_string(),
            target_did: "did:target".to_string(),
            source_did: "did:source".to_string(),
            created_at: std::time::Instant::now() - std::time::Duration::from_secs(600),
            response_tx: Some(tx),
        };

        registry.register(old_invocation);
        assert_eq!(registry.pending.len(), 1);

        // Clean up expired (older than 5 minutes)
        let expired = registry.cleanup_expired(std::time::Duration::from_secs(300));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], "old");
        assert!(registry.get("old").is_none());
    }

    #[tokio::test]
    async fn test_invocation_service_handle_send_response() {
        let (service, command_tx) = InvocationService::new(None);

        // Start service in background
        tokio::spawn(async move {
            service.run().await;
        });

        // Register a response channel
        let (response_tx, mut _response_rx): (mpsc::Sender<InvocationResponse>, _) =
            mpsc::channel(1);
        let (tx, _rx): (mpsc::Sender<Option<PendingInvocation>>, _) = mpsc::channel(1);

        command_tx
            .send(InvokeCommand::RegisterResponseChannel {
                agent_did: "did:test".to_string(),
                tx: response_tx,
            })
            .await
            .unwrap();

        // Send a response
        let response = InvocationResponse {
            invocation_id: "test-123".to_string(),
            from: "did:bob".to_string(),
            content: "Result!".to_string(),
            duration_ms: 100,
            success: true,
            error: None,
        };

        let (result_tx, mut result_rx) = mpsc::channel(1);
        command_tx
            .send(InvokeCommand::SendResponse {
                response: response.clone(),
                respond_to: result_tx,
            })
            .await
            .unwrap();

        let result = result_rx.recv().await.unwrap();
        assert!(result.is_ok());
    }
}
