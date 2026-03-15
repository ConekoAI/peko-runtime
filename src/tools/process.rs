//! Process execution tool for running commands
//!
//! Supports both sync and async execution modes:
//! - **Sync mode** (default): Blocks until command completes, returns result immediately
//! - **Async mode**: Returns receipt immediately, executes in background, delivers result via event queue

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use uuid::Uuid;

use crate::agent::async_tool_framework::{
    AsyncResultDeliveryMode, AsyncTaskResult, AsyncToolConfig, UnifiedAsyncExecutor,
};
use crate::tools::Tool;

/// Execution mode for process tool
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessMode {
    /// Synchronous: block until completion (default)
    Sync {
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
    /// Asynchronous: return receipt, execute in background
    Async {
        /// Optional label for the task
        #[serde(default)]
        label: Option<String>,
        /// Delivery mode for result
        #[serde(default)]
        delivery_mode: AsyncResultDeliveryMode,
    },
}

fn default_timeout() -> u64 {
    120
}

/// Process execution tool for running shell commands
pub struct ProcessTool {
    /// Default timeout in seconds
    default_timeout_secs: u64,
    /// Whether to allow shell commands (sh -c)
    allow_shell: bool,
    /// Unified async executor (for async mode)
    executor: Option<UnifiedAsyncExecutor>,
    /// Parent session key (for async result routing)
    session_key: Option<String>,
}

/// Maximum allowed timeout in seconds (5 minutes)
const MAX_TIMEOUT_SECS: u64 = 300;
/// Default timeout in seconds (2 minutes)
const DEFAULT_TIMEOUT_SECS: u64 = 120;

impl ProcessTool {
    /// Create a new process tool with default settings
    #[must_use]
    pub fn new() -> Self {
        Self {
            default_timeout_secs: DEFAULT_TIMEOUT_SECS,
            allow_shell: false,
            executor: None,
            session_key: None,
        }
    }

    /// Create with custom timeout
    #[must_use]
    pub fn with_timeout(timeout_secs: u64) -> Self {
        Self {
            default_timeout_secs: timeout_secs.min(MAX_TIMEOUT_SECS),
            allow_shell: false,
            executor: None,
            session_key: None,
        }
    }

    /// Create with shell support enabled
    #[must_use]
    pub fn with_shell() -> Self {
        Self {
            default_timeout_secs: 30,
            allow_shell: true,
            executor: None,
            session_key: None,
        }
    }

    /// Enable async mode with unified executor
    #[must_use]
    pub fn with_async(
        mut self,
        executor: UnifiedAsyncExecutor,
        session_key: impl Into<String>,
    ) -> Self {
        self.executor = Some(executor);
        self.session_key = Some(session_key.into());
        self
    }

    /// Execute a command with arguments
    async fn execute_command(
        command: &str,
        args: Vec<String>,
        timeout_secs: u64,
        working_dir: Option<&str>,
        env_vars: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<serde_json::Value> {
        let mut cmd = Command::new(command);
        cmd.args(&args);

        // Set working directory if provided
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Set environment variables if provided
        if let Some(envs) = env_vars {
            for (key, value) in envs {
                if let Some(val_str) = value.as_str() {
                    cmd.env(key, val_str);
                }
            }
        }

        // Execute with timeout
        let result = timeout(Duration::from_secs(timeout_secs), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);
                let success = output.status.success();

                // Truncate output if too large (over 100KB)
                let stdout = if stdout.len() > 100_000 {
                    format!("{}...(truncated)", &stdout[..100_000])
                } else {
                    stdout
                };

                let stderr = if stderr.len() > 100_000 {
                    format!("{}...(truncated)", &stderr[..100_000])
                } else {
                    stderr
                };

                Ok(json!({
                    "command": command,
                    "args": args,
                    "stdout": stdout,
                    "stderr": stderr,
                    "exit_code": exit_code,
                    "success": success,
                }))
            }
            Ok(Err(e)) => Err(anyhow::anyhow!(
                "Failed to execute command '{command}': {e}"
            )),
            Err(_) => Err(anyhow::anyhow!(
                "Command '{command}' timed out after {timeout_secs} seconds"
            )),
        }
    }

    /// Execute a shell command (if enabled)
    async fn execute_shell(
        command: &str,
        timeout_secs: u64,
        working_dir: Option<&str>,
    ) -> Result<serde_json::Value> {
        Self::execute_command(
            "sh",
            vec!["-c".to_string(), command.to_string()],
            timeout_secs,
            working_dir,
            None,
        )
        .await
    }

    /// Execute command in async mode using UnifiedAsyncExecutor
    async fn execute_async(
        &self,
        command: String,
        args: Vec<String>,
        timeout_secs: u64,
        working_dir: Option<String>,
        env_vars: Option<serde_json::Map<String, serde_json::Value>>,
        label: Option<String>,
        delivery_mode: AsyncResultDeliveryMode,
    ) -> Result<serde_json::Value> {
        let executor = self
            .executor
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Async mode not configured for process tool"))?;

        let session_key = self
            .session_key
            .clone()
            .unwrap_or_else(|| "default".to_string());

        let task_id = format!("process_{}", Uuid::new_v4().simple());

        // Clone values for the execution closure
        let command_clone = command.clone();
        let working_dir_clone = working_dir.clone();

        // Execute using unified executor
        let receipt = executor
            .execute(
                task_id.clone(),
                "process",
                json!({
                    "command": &command,
                    "args": &args,
                    "working_dir": &working_dir,
                }),
                session_key,
                AsyncToolConfig {
                    delivery_mode,
                    delivery_target: None,
                    timeout_secs,
                    cleanup_after_delivery: true,
                    label: label.clone(),
                },
                move || async move {
                    // Execute the command
                    let result = Self::execute_command(
                        &command_clone,
                        args,
                        timeout_secs,
                        working_dir_clone.as_deref(),
                        env_vars,
                    )
                    .await;

                    // Convert to AsyncTaskResult
                    match result {
                        Ok(output) => {
                            let stdout = output["stdout"].as_str().unwrap_or("").to_string();
                            let stderr = output["stderr"].as_str().unwrap_or("").to_string();
                            let exit_code = output["exit_code"].as_i64().unwrap_or(-1) as i32;
                            Ok(AsyncTaskResult::Process {
                                stdout,
                                stderr,
                                exit_code,
                            })
                        }
                        Err(e) => Err(e),
                    }
                },
            )
            .await?;

        // Return receipt
        Ok(json!({
            "task_id": receipt.task_id,
            "status": "accepted",
            "mode": "async",
            "command": command,
            "check_status_tool": receipt.check_status_tool,
        }))
    }
}

impl Default for ProcessTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> &'static str {
        "process"
    }

    fn description(&self) -> &'static str {
        "Execute system commands with arguments, timeout, and working directory"
    }

    fn llm_description(&self) -> String {
        r#"## Purpose
Execute system commands with sync or async support. For build commands, git operations, system diagnostics, and long-running tasks.

## Modes

### Sync Mode (default)
Blocks until command completes. Use for quick commands that return immediately.
```json
{"command": "git", "args": ["status"]}
```

### Async Mode
Returns immediately with task ID. Command runs in background, result delivered via event system.
Use for long-running builds, tests, downloads, or when you need to run multiple commands in parallel.
```json
{
    "command": "cargo",
    "args": ["build", "--release"],
    "mode": "async",
    "label": "release-build"
}
```

## When to Use
- Running build commands (cargo build, npm install, make, etc.)
- Git operations (status, log, diff, commit when authorized)
- System diagnostics (df, ps, netstat, etc.)
- File operations that need scripting (find with complex filters)
- Running tests or linting tools
- Long-running downloads (use async mode or timeout: 0)

## When NOT to Use
- Simple file reads/writes (use `filesystem` instead)
- Code patches that preserve file structure (use `apply_patch` instead)
- Destructive operations without explicit user confirmation
- Commands with unbounded output (may be truncated)

## Input (Sync Mode)
```json
{
  "command": "command-name",
  "args": ["arg1", "arg2"],
  "timeout": 120,
  "working_dir": "/optional/path",
  "env": {"KEY": "value"}
}
```

## Input (Async Mode)
```json
{
  "command": "command-name",
  "args": ["arg1", "arg2"],
  "mode": "async",
  "label": "optional-label",
  "timeout": 300
}
```

## Timeout Behavior
- **Default**: 120 seconds (suitable for most commands)
- **Short tasks** (date, echo, ls): Use default or timeout: 5
- **Build commands**: Use timeout: 120 or higher (or async mode)
- **Long downloads**: Use async mode or timeout: 0 (disables timeout)
- **Max**: 300 seconds (5 minutes) unless timeout is 0

## Returns (Sync Mode)
- stdout and stderr output (truncated if >100KB)
- Exit code
- Success/failure status
- Timeout error message if command times out

## Returns (Async Mode)
- task_id: Unique identifier for tracking
- status: "accepted"
- mode: "async"
- check_status_tool: Tool name to check status later

## Result Delivery (Async Mode)
Results are delivered via the async event system when the command completes.

## Examples

### Sync Mode Examples
Check git status:
```json
{"command": "git", "args": ["status"]}
```

Build a Rust project:
```json
{"command": "cargo", "args": ["build", "--release"], "timeout": 300}
```

### Async Mode Examples
Long build in background:
```json
{
    "command": "cargo",
    "args": ["build", "--release"],
    "mode": "async",
    "label": "building-release"
}
```

Run tests without blocking:
```json
{
    "command": "cargo",
    "args": ["test", "--all"],
    "mode": "async",
    "label": "running-tests",
    "timeout": 300
}
```

## Safety
- Commands are validated for forbidden characters
- Timeout prevents runaway processes (default: 120s, max: 300s, or unlimited with timeout: 0)
- Output is truncated at 100KB to prevent memory issues
- Prefer `trash` over `rm` for recoverable deletes
- Ask before destructive operations"#.to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "args": {
                    "type": "array",
                    "description": "Command arguments",
                    "items": { "type": "string" }
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120). Set to 0 to disable timeout for long-running tasks like downloads. Max: 300 unless timeout is 0.",
                    "minimum": 0,
                    "maximum": 300
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory for the command"
                },
                "env": {
                    "type": "object",
                    "description": "Environment variables"
                },
                "mode": {
                    "type": "string",
                    "enum": ["sync", "async"],
                    "description": "Execution mode: 'sync' blocks for result, 'async' returns receipt and runs in background (default: sync)"
                },
                "label": {
                    "type": "string",
                    "description": "Optional label for async mode (for identifying the task)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        // Check if this is a shell command
        if let Some(shell_cmd) = params.get("shell").and_then(|v| v.as_str()) {
            if !self.allow_shell {
                return Err(anyhow::anyhow!(
                    "Shell execution not enabled. Use command and args instead."
                ));
            }

            let timeout_secs = params
                .get("timeout")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(self.default_timeout_secs);

            let working_dir = params.get("working_dir").and_then(|v| v.as_str());

            return Self::execute_shell(shell_cmd, timeout_secs, working_dir).await;
        }

        // Regular command execution
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: command"))?;

        // Security: validate command name
        let forbidden_chars = [';', '|', '&', '$', '`', '\'', '"'];
        if command.chars().any(|c| forbidden_chars.contains(&c)) {
            return Err(anyhow::anyhow!("Command contains forbidden characters"));
        }

        let args: Vec<String> = params
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let timeout_secs = params
            .get("timeout")
            .and_then(serde_json::Value::as_u64)
            .map_or(self.default_timeout_secs, |t| {
                if t == 0 {
                    u64::MAX
                } else {
                    t.min(MAX_TIMEOUT_SECS)
                }
            });

        let working_dir = params.get("working_dir").and_then(|v| v.as_str());
        let env_vars = params.get("env").and_then(|v| v.as_object()).cloned();

        // Determine execution mode
        let mode_str = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("sync");

        match mode_str {
            "async" => {
                let label = params
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let delivery_mode = AsyncResultDeliveryMode::default();

                self.execute_async(
                    command.to_string(),
                    args,
                    timeout_secs,
                    working_dir.map(String::from),
                    env_vars,
                    label,
                    delivery_mode,
                )
                .await
            }
            "sync" | _ => {
                // Default sync mode
                Self::execute_command(command, args, timeout_secs, working_dir, env_vars).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::async_tool_framework::{AsyncResultQueueManager, AsyncTaskRegistry};
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn test_process_tool_creation() {
        let tool = ProcessTool::new();
        assert_eq!(tool.name(), "process");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_process_tool_with_async_support() {
        let registry = Arc::new(RwLock::new(AsyncTaskRegistry::new()));
        let queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let executor = UnifiedAsyncExecutor::with_registries(registry, queue_manager);

        let tool = ProcessTool::new().with_async(executor, "test_session");

        assert!(tool.executor.is_some());
        assert_eq!(tool.session_key, Some("test_session".to_string()));
    }

    #[tokio::test]
    async fn test_process_async_mode() {
        let registry = Arc::new(RwLock::new(AsyncTaskRegistry::new()));
        let queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let executor =
            UnifiedAsyncExecutor::with_registries(registry.clone(), queue_manager.clone());

        let tool = ProcessTool::new().with_async(executor, "test_session");

        let params = json!({
            "command": "echo",
            "args": ["Hello from async"],
            "mode": "async",
            "label": "test-echo"
        });

        let result = tool.execute(params).await;
        assert!(
            result.is_ok(),
            "Async process execution failed: {:?}",
            result
        );

        let response = result.unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "accepted");
        assert_eq!(response["mode"].as_str().unwrap(), "async");
        assert!(response["task_id"].as_str().is_some());
        assert_eq!(
            response["check_status_tool"].as_str().unwrap(),
            "async_task_status"
        );

        let task_id = response["task_id"].as_str().unwrap().to_string();

        // Wait a bit for the task to complete
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify task was registered and completed
        let reg = registry.read().await;
        let status = reg.check_status(&task_id);
        assert!(status.is_some());
        assert!(status.unwrap().is_terminal());
    }

    #[tokio::test]
    async fn test_process_async_mode_not_configured() {
        // Tool without async support should fail in async mode
        let tool = ProcessTool::new();

        let params = json!({
            "command": "echo",
            "args": ["test"],
            "mode": "async"
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Async mode not configured"));
    }

    #[tokio::test]
    async fn test_process_async_with_timeout() {
        let registry = Arc::new(RwLock::new(AsyncTaskRegistry::new()));
        let queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let executor =
            UnifiedAsyncExecutor::with_registries(registry.clone(), queue_manager.clone());

        let tool = ProcessTool::new().with_async(executor, "test_session");

        let params = json!({
            "command": "sleep",
            "args": ["0.5"],
            "mode": "async",
            "timeout": 1,
            "label": "short-sleep"
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let task_id = result.unwrap()["task_id"].as_str().unwrap().to_string();

        // Wait for task to complete
        tokio::time::sleep(Duration::from_secs(1)).await;

        let reg = registry.read().await;
        let entry = reg.get(&task_id);
        assert!(entry.is_some());
        assert!(entry.unwrap().status.is_terminal());
    }

    #[tokio::test]
    async fn test_process_echo() {
        let tool = ProcessTool::new();
        let params = json!({
            "command": "echo",
            "args": ["Hello", "World"]
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.get("success").unwrap().as_bool().unwrap());
        assert!(response
            .get("stdout")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("Hello World"));
    }

    #[tokio::test]
    async fn test_process_exit_code() {
        let tool = ProcessTool::new();

        // Command that succeeds
        let params = json!({
            "command": "true"
        });
        let result = tool.execute(params).await.unwrap();
        assert!(result.get("success").unwrap().as_bool().unwrap());
        assert_eq!(result.get("exit_code").unwrap().as_i64().unwrap(), 0);

        // Command that fails
        let params = json!({
            "command": "false"
        });
        let result = tool.execute(params).await.unwrap();
        assert!(!result.get("success").unwrap().as_bool().unwrap());
        assert_ne!(result.get("exit_code").unwrap().as_i64().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_process_stderr() {
        let tool = ProcessTool::new();
        let params = json!({
            "command": "ls",
            "args": ["/nonexistent_directory_that_does_not_exist"]
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        let stderr = response.get("stderr").unwrap().as_str().unwrap();
        assert!(!stderr.is_empty() || !response.get("success").unwrap().as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_process_timeout() {
        let tool = ProcessTool::new();
        let params = json!({
            "command": "sleep",
            "args": ["10"],
            "timeout": 1
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_process_timeout_disabled() {
        let tool = ProcessTool::new();
        // timeout: 0 should disable timeout, allowing long commands to complete
        let params = json!({
            "command": "sleep",
            "args": ["0.5"],
            "timeout": 0
        });

        let result = tool.execute(params).await;
        // Should succeed because timeout is disabled
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response.get("success").unwrap().as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_process_timeout_max_cap() {
        let tool = ProcessTool::new();
        // timeout > 300 should be capped at 300
        let params = json!({
            "command": "echo",
            "args": ["hello"],
            "timeout": 9999  // Way over max, should be capped
        });

        // Should still work (not timeout immediately)
        let result = tool.execute(params).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response.get("success").unwrap().as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_process_working_dir() {
        let tool = ProcessTool::new();
        let params = json!({
            "command": "pwd",
            "working_dir": "/tmp"
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        let stdout = response.get("stdout").unwrap().as_str().unwrap();
        assert!(stdout.contains("/tmp") || stdout.contains("tmp"));
    }

    #[tokio::test]
    async fn test_process_env_vars() {
        let tool = ProcessTool::new();
        let params = json!({
            "command": "sh",
            "args": ["-c", "echo $TEST_VAR"],
            "env": {
                "TEST_VAR": "hello_world"
            }
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        let stdout = response.get("stdout").unwrap().as_str().unwrap();
        assert!(stdout.contains("hello_world"));
    }

    #[tokio::test]
    async fn test_process_forbidden_chars() {
        let tool = ProcessTool::new();
        let params = json!({
            "command": "echo; rm -rf /"
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("forbidden"));
    }

    #[tokio::test]
    async fn test_process_shell_disabled() {
        let tool = ProcessTool::new();
        let params = json!({
            "shell": "echo hello"
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not enabled"));
    }

    #[tokio::test]
    async fn test_process_shell_enabled() {
        let tool = ProcessTool::with_shell();
        let params = json!({
            "shell": "echo hello_world"
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.get("success").unwrap().as_bool().unwrap());
        assert!(response
            .get("stdout")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("hello_world"));
    }
}
