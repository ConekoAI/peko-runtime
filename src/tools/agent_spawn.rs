//! Agent Spawn Tool (OpenClaw-Style)
//!
//! Spawns subagent sessions for isolated task execution.
//! Results are announced back to the parent via the event system.
//!
//! Note: Async execution and timeout are handled by the framework-level
//! `ToolWrapper` using `_async` and `_timeout` parameters.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::agent::subagent_error::SpawnError;
use crate::agent::subagent_executor::{ExecutionConfig, SubagentExecutor};
use crate::agent::subagent_registry::{
    find_run_across_all_registries, list_all_runs_across_all_registries, SharedSubagentRegistry,
};
use crate::session::types::SpawnCleanupPolicy;
use crate::tools::Tool;

/// Maximum allowed spawn depth (safety limit)
const DEFAULT_MAX_SPAWN_DEPTH: u32 = 3;

/// Maximum concurrent subagent runs per agent
const DEFAULT_MAX_CONCURRENT: usize = 5;

/// Trait for providing the current session key
///
/// This allows the tool to get the current session key at execution time,
/// even though the session is determined at runtime.
pub trait SessionKeyProvider: Send + Sync {
    /// Get the current session key
    fn current_session_key(&self) -> String;
}

/// Simple session key provider that returns a static key
pub struct StaticSessionKeyProvider {
    session_key: String,
}

impl StaticSessionKeyProvider {
    #[must_use]
    pub fn new(session_key: impl Into<String>) -> Self {
        Self {
            session_key: session_key.into(),
        }
    }
}

impl SessionKeyProvider for StaticSessionKeyProvider {
    fn current_session_key(&self) -> String {
        self.session_key.clone()
    }
}

impl SessionKeyProvider for Arc<DynamicSessionKeyProvider> {
    fn current_session_key(&self) -> String {
        self.get_session_key()
    }
}

/// Dynamic session key provider that can be updated at runtime
///
/// This is useful when the session key is determined at runtime,
/// such as when processing messages from different sessions.
#[derive(Clone)]
pub struct DynamicSessionKeyProvider {
    session_key: Arc<std::sync::RwLock<String>>,
}

impl DynamicSessionKeyProvider {
    #[must_use]
    pub fn new(initial_key: impl Into<String>) -> Self {
        Self {
            session_key: Arc::new(std::sync::RwLock::new(initial_key.into())),
        }
    }

    /// Update the current session key
    pub fn set_session_key(&self, key: impl Into<String>) {
        if let Ok(mut guard) = self.session_key.write() {
            *guard = key.into();
        }
    }

    /// Get the current session key
    #[must_use]
    pub fn get_session_key(&self) -> String {
        self.session_key
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

impl SessionKeyProvider for DynamicSessionKeyProvider {
    fn current_session_key(&self) -> String {
        self.get_session_key()
    }
}

/// Agent Spawn Arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpawnArgs {
    /// Task description for the subagent
    pub task: String,
    /// Optional label for tracking
    pub label: Option<String>,
    /// Create isolated session without parent context
    #[serde(default)]
    pub isolated: bool,
    /// Cleanup policy: "keep" or "delete"
    #[serde(default)]
    pub cleanup: Option<String>,
    /// Parent session key (auto-detected if not provided)
    pub parent_session_key: Option<String>,
}

/// Agent Spawn Tool
///
/// Creates a subagent session and executes a task in the background.
/// Results are announced back to the parent when complete.
pub struct AgentSpawnTool {
    /// Subagent executor for background execution
    executor: Arc<SubagentExecutor>,
    /// Session key provider to get current session at execution time
    session_provider: Option<Box<dyn SessionKeyProvider>>,
    /// Maximum spawn depth allowed
    max_depth: u32,
    /// Maximum concurrent runs
    max_concurrent: usize,
}

impl AgentSpawnTool {
    /// Create a new spawn tool with an executor
    #[must_use]
    pub fn new(executor: Arc<SubagentExecutor>) -> Self {
        Self {
            executor,
            session_provider: None,
            max_depth: DEFAULT_MAX_SPAWN_DEPTH,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    /// Create a spawn tool with a session key provider
    #[must_use]
    pub fn with_session_provider(
        executor: Arc<SubagentExecutor>,
        provider: Box<dyn SessionKeyProvider>,
    ) -> Self {
        Self {
            executor,
            session_provider: Some(provider),
            max_depth: DEFAULT_MAX_SPAWN_DEPTH,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    /// Set maximum spawn depth
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set maximum concurrent runs
    #[must_use]
    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = max_concurrent;
        self
    }

    /// Get the executor
    #[must_use]
    pub fn executor(&self) -> &Arc<SubagentExecutor> {
        &self.executor
    }

    /// Execute subagent spawn in async mode (returns receipt)
    async fn execute_spawn_async(
        &self,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
        config: ExecutionConfig,
        label: Option<String>,
        cleanup: SpawnCleanupPolicy,
    ) -> anyhow::Result<serde_json::Value> {
        // Extract timeout before config is moved
        let timeout_seconds = config.timeout_seconds;

        match self
            .executor
            .spawn_and_execute(
                task,
                None,
                isolated,
                parent_session_key,
                config,
            )
            .await
        {
            Ok(run_id) => {
                // Get the run info to return the child session key
                let registry = self.executor.registry().read().await;
                let run = registry.get(&run_id).ok_or_else(|| {
                    anyhow::anyhow!("Run {run_id} not found in registry after spawn")
                })?;

                let child_session_key = run.child_session_key.clone();

                // Return receipt-style response for async mode
                Ok(json!({
                    "status": "accepted",
                    "childSessionKey": child_session_key,
                    "runId": run_id,
                    "note": "Subagent is running in the background. Use agent_spawn_status with the runId to check progress.",
                    "label": label,
                    "isolated": isolated,
                    "timeout_seconds": timeout_seconds,
                    "cleanup": match cleanup {
                        SpawnCleanupPolicy::Keep => "keep",
                        SpawnCleanupPolicy::Delete => "delete",
                    }
                }))
            }
            Err(e) => Self::format_error_response(&e),
        }
    }

    /// Execute subagent spawn in blocking mode (waits for completion, returns inline result)
    async fn execute_spawn_blocking(
        &self,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
        config: ExecutionConfig,
        label: Option<String>,
        cleanup: SpawnCleanupPolicy,
    ) -> anyhow::Result<serde_json::Value> {
        let timeout_seconds = config.timeout_seconds;

        match self
            .executor
            .execute_and_wait(
                task,
                None,
                isolated,
                parent_session_key,
                config,
                timeout_seconds,
            )
            .await
        {
            Ok(run) => {
                // Return inline result — the subagent's output is available immediately
                let status_str = run.status.as_str();
                let success = matches!(run.status, crate::agent::subagent_registry::SubagentStatus::Completed { .. });

                let mut result = json!({
                    "status": status_str,
                    "run_id": run.run_id,
                    "child_session_key": run.child_session_key,
                    "success": success,
                    "label": label,
                    "isolated": isolated,
                    "timeout_seconds": timeout_seconds,
                    "cleanup": match cleanup {
                        SpawnCleanupPolicy::Keep => "keep",
                        SpawnCleanupPolicy::Delete => "delete",
                    }
                });

                // Include output or error if available
                if let Some(ref subagent_result) = run.result {
                    if let Some(ref output) = subagent_result.output {
                        result["output"] = json!(output);
                    }
                    if let Some(ref error) = subagent_result.error {
                        result["error"] = json!(error);
                    }
                }

                Ok(result)
            }
            Err(e) => Self::format_error_response(&e),
        }
    }

    /// Format error response
    ///
    /// Classifies the error using a typed [`SpawnError`] when available,
    /// falling back to string matching only for untyped errors.
    fn format_error_response(error: &anyhow::Error) -> anyhow::Result<serde_json::Value> {
        // Try typed classification first
        if let Some(spawn_err) = error.downcast_ref::<SpawnError>() {
            return match spawn_err {
                SpawnError::DepthLimitExceeded { .. } => Ok(json!({
                    "status": "forbidden",
                    "error": spawn_err.to_string(),
                    "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth."
                })),
                SpawnError::ConcurrentLimitExceeded { .. } => Ok(json!({
                    "status": "forbidden",
                    "error": spawn_err.to_string(),
                    "note": "Maximum concurrent subagent runs exceeded. Please wait for existing runs to complete."
                })),
                SpawnError::Timeout { .. } => Ok(json!({
                    "status": "timeout",
                    "error": spawn_err.to_string(),
                    "note": "Subagent execution timed out."
                })),
                SpawnError::ExecutionFailed(msg) => Ok(json!({
                    "status": "error",
                    "error": msg,
                })),
            };
        }

        // Fallback to string matching for untyped errors
        let error_msg = error.to_string();
        let lower_msg = error_msg.to_lowercase();
        if lower_msg.contains("depth") {
            Ok(json!({
                "status": "forbidden",
                "error": error_msg,
                "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth."
            }))
        } else if lower_msg.contains("concurrent") {
            Ok(json!({
                "status": "forbidden",
                "error": error_msg,
                "note": "Maximum concurrent subagent runs exceeded. Please wait for existing runs to complete."
            }))
        } else if lower_msg.contains("timeout") || lower_msg.contains("timed out") {
            Ok(json!({
                "status": "timeout",
                "error": error_msg,
                "note": "Subagent execution timed out."
            }))
        } else {
            Ok(json!({
                "status": "error",
                "error": error_msg
            }))
        }
    }
}

#[async_trait]
impl Tool for AgentSpawnTool {
    fn name(&self) -> &'static str {
        "agent_spawn"
    }

    fn description(&self) -> String {
        r#"Spawn a sub-agent run in an isolated or shared session.

Default mode (blocking): The parent waits for the subagent to complete its agentic loop and returns the result inline.

Async mode: Use `_async: true` to spawn the subagent in the background. A receipt is returned immediately. Use `agent_spawn_status` with the runId to check progress.

Parameters:
- task: Description of the task to execute (required)
- label: Label for this spawn (optional)
- isolated: If true, creates isolated session without parent context (default: false)
- cleanup: "keep" or "delete" - what to do with session after completion (default: "keep")
- parent_session_key: Parent session key (optional - auto-detected if not provided)

Examples:
// Blocking spawn (default) - parent waits for result
{"task": "Use write_file to create report.txt with a summary"}

// Async spawn - background execution
{"task": "Long running analysis", "_async": true, "_timeout": 300}

// Isolated context - fresh session
{"task": "Analyze confidential data", "isolated": true, "cleanup": "delete"}"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Description of the task to execute"
                },
                "label": {
                    "type": "string",
                    "description": "Optional label for tracking this spawn"
                },
                "isolated": {
                    "type": "boolean",
                    "description": "If true, creates isolated session without parent context",
                    "default": false
                },
                "cleanup": {
                    "type": "string",
                    "description": "What to do with session after completion: 'keep' or 'delete'",
                    "default": "keep"
                },
                "parent_session_key": {
                    "type": "string",
                    "description": "Parent session key (auto-detected if not provided)"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, mut params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Check for _async reserved parameter (extracted by AsyncExecutionRouter,
        // but we also check here for direct tool calls that bypass the router)
        let async_mode = params.get("_async")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Parse parameters (after _async extraction, the rest are tool-specific)
        let args: AgentSpawnArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let cleanup = args.cleanup.map_or(SpawnCleanupPolicy::Keep, |s| {
            match s.to_lowercase().as_str() {
                "delete" => SpawnCleanupPolicy::Delete,
                _ => SpawnCleanupPolicy::Keep,
            }
        });

        // Get parent session key - from params or session provider
        let parent_session_key = if let Some(key) = args.parent_session_key {
            key
        } else if let Some(ref provider) = self.session_provider {
            provider.current_session_key()
        } else {
            return Err(anyhow::anyhow!(
                "AgentSpawnTool requires a parent_session_key parameter or session provider. \
                Please provide parent_session_key in the tool parameters."
            ));
        };

        // Build execution config with defaults
        let config = ExecutionConfig {
            timeout_seconds: 300, // Default timeout, can be overridden by framework _timeout
            cleanup,
            label: args.label.clone(),
            announce_completion: true,
            max_depth: self.max_depth,
        };

        // Route based on async mode
        if async_mode {
            // Async mode: spawn in background, return receipt
            self.execute_spawn_async(
                &args.task,
                args.isolated,
                &parent_session_key,
                config,
                args.label,
                cleanup,
            )
            .await
        } else {
            // Blocking mode (default): wait for subagent to complete, return inline result
            self.execute_spawn_blocking(
                &args.task,
                args.isolated,
                &parent_session_key,
                config,
                args.label,
                cleanup,
            )
            .await
        }
    }
}

/// Tool for checking subagent run status
///
/// Allows checking the status of a previously spawned subagent.
pub struct AgentSpawnStatusTool {
    /// Optional bound registry for per-agent usage.
    /// When `None`, the tool searches across all agent registries (global mode).
    registry: Option<SharedSubagentRegistry>,
}

impl AgentSpawnStatusTool {
    /// Create a status tool bound to a specific agent's registry.
    #[must_use]
    pub fn with_registry(registry: SharedSubagentRegistry) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    /// Create a global status tool that searches across all agent registries.
    #[must_use]
    pub fn global() -> Self {
        Self { registry: None }
    }

    /// Look up a run by ID, either in the bound registry or across all registries.
    async fn lookup_run(&self, run_id: &str) -> Option<crate::agent::subagent_registry::SubagentRun> {
        match &self.registry {
            Some(registry) => {
                let reg = registry.read().await;
                reg.get(run_id).cloned()
            }
            None => find_run_across_all_registries(run_id).await,
        }
    }
}

#[async_trait]
impl Tool for AgentSpawnStatusTool {
    fn name(&self) -> &'static str {
        "agent_spawn_status"
    }

    fn description(&self) -> String {
        r"Check the status of a previously spawned subagent.

Parameters:
- run_id: The run ID returned by agent_spawn (required)

Returns the current status and result if complete."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "The run ID returned by agent_spawn (required)"
                }
            },
            "required": ["run_id"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let run_id = params
            .get("run_id")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'run_id' parameter"))?;

        match self.lookup_run(run_id).await {
            Some(run) => {
                let status_str = run.status.as_str();
                let is_terminal = run.status.is_terminal();

                let mut response = json!({
                    "run_id": run_id,
                    "status": status_str,
                    "is_terminal": is_terminal,
                    "child_session_key": run.child_session_key,
                    "parent_session_key": run.parent_session_key,
                    "task": run.task,
                    "label": run.label,
                    "depth": run.depth,
                    "started_at": run.started_at.to_rfc3339(),
                });

                // Add result if terminal
                if let Some(ref result) = run.result {
                    if let Some(obj) = response.as_object_mut() {
                        obj.insert("output".to_string(), json!(result.output));
                        obj.insert("error".to_string(), json!(result.error));
                        obj.insert(
                            "completed_at".to_string(),
                            json!(result.completed_at.to_rfc3339()),
                        );
                    }
                }

                // Add duration
                if let Some(duration) = run.duration() {
                    if let Some(obj) = response.as_object_mut() {
                        obj.insert(
                            "duration_seconds".to_string(),
                            json!(duration.num_seconds()),
                        );
                    }
                }

                Ok(response)
            }
            None => Ok(json!({
                "error": "Run not found",
                "run_id": run_id
            })),
        }
    }
}

/// Tool for listing active subagent runs
///
/// Shows all subagents spawned from the current session.
pub struct AgentSpawnListTool {
    /// Optional bound registry for per-agent usage.
    /// When `None`, the tool lists runs across all agent registries (global mode).
    registry: Option<SharedSubagentRegistry>,
}

impl AgentSpawnListTool {
    /// Create a list tool bound to a specific agent's registry.
    #[must_use]
    pub fn with_registry(registry: SharedSubagentRegistry) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    /// Create a global list tool that searches across all agent registries.
    #[must_use]
    pub fn global() -> Self {
        Self { registry: None }
    }

    /// List runs, either from the bound registry or across all registries.
    async fn list_runs(&self) -> Vec<crate::agent::subagent_registry::SubagentRun> {
        match &self.registry {
            Some(registry) => {
                let reg = registry.read().await;
                reg.list_all().into_iter().cloned().collect()
            }
            None => list_all_runs_across_all_registries().await,
        }
    }
}

#[async_trait]
impl Tool for AgentSpawnListTool {
    fn name(&self) -> &'static str {
        "agent_spawn_list"
    }

    fn description(&self) -> String {
        "List all active subagent runs for the current session.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let runs = self.list_runs().await;
        let run_jsons: Vec<_> = runs
            .into_iter()
            .map(|run| {
                json!({
                    "run_id": run.run_id,
                    "status": run.status.as_str(),
                    "task": run.task,
                    "label": run.label,
                    "depth": run.depth,
                    "started_at": run.started_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(json!({
            "total": run_jsons.len(),
            "active": run_jsons.iter().filter(|r| !r["status"].as_str().unwrap_or("").eq("completed") && !r["status"].as_str().unwrap_or("").eq("failed") && !r["status"].as_str().unwrap_or("").eq("cancelled") && !r["status"].as_str().unwrap_or("").eq("timed_out")).count(),
            "runs": run_jsons
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::manager::SessionManager;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_spawn_tool_creation() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = Arc::new(SubagentExecutor::new(manager, "test_agent", 5));
        let tool = AgentSpawnTool::new(executor);

        assert_eq!(tool.name(), "agent_spawn");
    }

    #[tokio::test]
    async fn test_spawn_tool_with_session_provider() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = Arc::new(SubagentExecutor::new(manager, "test_agent", 5));

        let provider = Box::new(StaticSessionKeyProvider::new("test:session:key"));
        let tool = AgentSpawnTool::with_session_provider(executor, provider);

        assert_eq!(tool.name(), "agent_spawn");
    }

    #[test]
    fn test_default_max_depth() {
        assert_eq!(DEFAULT_MAX_SPAWN_DEPTH, 3);
    }

    #[test]
    fn test_default_max_concurrent() {
        assert_eq!(DEFAULT_MAX_CONCURRENT, 5);
    }

    #[tokio::test]
    async fn test_async_mode_error_response_formatting() {
        // Test typed depth error
        let depth_err = anyhow::anyhow!(SpawnError::DepthLimitExceeded {
            current: 4,
            max: 3,
        });
        let response = AgentSpawnTool::format_error_response(&depth_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "forbidden");
        assert!(response["note"].as_str().unwrap().contains("depth"));
        assert!(response["error"].as_str().unwrap().contains("4"));

        // Test typed concurrent error
        let concurrent_err = anyhow::anyhow!(SpawnError::ConcurrentLimitExceeded {
            current: 5,
            max: 5,
        });
        let response = AgentSpawnTool::format_error_response(&concurrent_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "forbidden");
        assert!(response["note"].as_str().unwrap().contains("concurrent"));

        // Test typed timeout error
        let timeout_err = anyhow::anyhow!(SpawnError::Timeout { seconds: 30 });
        let response = AgentSpawnTool::format_error_response(&timeout_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "timeout");
        assert!(response["error"].as_str().unwrap().contains("30"));

        // Test typed execution failed error
        let exec_err = anyhow::anyhow!(SpawnError::ExecutionFailed("something went wrong".to_string()));
        let response = AgentSpawnTool::format_error_response(&exec_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "error");
        assert!(response["error"].as_str().unwrap().contains("something went wrong"));

        // Test fallback string matching for untyped errors
        let untyped = anyhow::anyhow!("Some random depth-related failure");
        let response = AgentSpawnTool::format_error_response(&untyped).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "forbidden");
        assert!(response["note"].as_str().unwrap().contains("depth"));
    }

    #[test]
    fn test_args_parsing() {
        let json = r#"{
            "task": "Do something",
            "label": "my-task",
            "isolated": true,
            "cleanup": "delete"
        }"#;

        let args: AgentSpawnArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.task, "Do something");
        assert_eq!(args.label, Some("my-task".to_string()));
        assert!(args.isolated);
        assert_eq!(args.cleanup, Some("delete".to_string()));
    }
}
