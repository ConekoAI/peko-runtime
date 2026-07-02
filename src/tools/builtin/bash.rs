//! `Bash` tool - Execute system shell commands
//!
//! Implements ADR-014: All-or-nothing permission model
//! - Full shell access via system shell (sh/bash on Unix, cmd on Windows)
//! - No sandboxing, no command blocking, no env filtering
//! - Security boundary is tool enablement (enabled = full access)
//!
//! Supports both blocking execution and `run_in_background` for parity with
//! Claude Code's `Bash` tool. Background tasks are tracked by the async executor
//! framework; poll them with the Async* family (AsyncOutput, AsyncStatus,
//! AsyncStop, AsyncList) — there is no implicit auto-detach in this tool;
//! blocking calls are bounded only by `timeout`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::RwLock;

use crate::extensions::framework::async_exec::executor::{
    get_or_create_registry_for_agent, AsyncExecutor, AsyncResultQueueManager,
};
use crate::extensions::framework::async_exec::AsyncToolConfig;
use crate::tools::core::{Tool, ToolContext};

/// Platform-specific shell configuration
#[cfg(unix)]
const SHELL: &str = "/bin/sh";
#[cfg(unix)]
const SHELL_ARG: &str = "-c";

#[cfg(windows)]
const SHELL: &str = "powershell";
#[cfg(windows)]
const SHELL_ARG: &str = "-Command";

/// Platform-specific shell name for display
#[cfg(unix)]
const SHELL_DISPLAY: &str = "/bin/sh";
#[cfg(windows)]
const SHELL_DISPLAY: &str = "PowerShell";

/// Platform name for display
const OS_DISPLAY: &str = if cfg!(windows) {
    "Windows"
} else {
    "Unix/Linux/macOS"
};

/// `Bash` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashArgs {
    /// Shell command to execute (passed directly to system shell)
    pub command: String,
    /// Optional human-readable description of the command (ignored by the tool,
    /// but useful for model reasoning and audit logs)
    #[serde(default)]
    pub description: Option<String>,
    /// Working directory (defaults to workspace if set)
    #[serde(default)]
    pub cwd: Option<String>,
    /// When true, run the command in the background and return a task receipt
    #[serde(default)]
    pub run_in_background: bool,
    /// Optional timeout in milliseconds for blocking execution.
    /// Ignored when `run_in_background` is true (use the async control
    /// family to cancel or monitor background tasks).
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// `Bash` tool - Execute system shell commands
pub struct BashTool {
    /// Workspace directory (default cwd)
    workspace_dir: Option<std::path::PathBuf>,
}

impl BashTool {
    /// Create a new `Bash` tool with default settings
    #[must_use]
    pub fn new() -> Self {
        Self {
            workspace_dir: None,
        }
    }

    /// Configure workspace directory (default working directory)
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<std::path::PathBuf>) -> Self {
        self.workspace_dir = Some(workspace.into());
        self
    }

    /// Resolve working directory
    fn resolve_cwd(&self, cwd: Option<&str>) -> Option<std::path::PathBuf> {
        cwd.map(std::path::PathBuf::from)
            .or_else(|| self.workspace_dir.clone())
    }

    /// Shared background executor for `run_in_background`.
    ///
    /// Uses a global registry keyed under the synthetic agent name `Bash` so
    /// the async-task-control family can find background shell tasks through
    /// the existing `find_task_across_all_registries` path.
    fn background_executor() -> Arc<AsyncExecutor> {
        use std::sync::OnceLock;
        static EXECUTOR: OnceLock<Arc<AsyncExecutor>> = OnceLock::new();
        EXECUTOR
            .get_or_init(|| {
                let registry = get_or_create_registry_for_agent("Bash");
                let queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
                Arc::new(AsyncExecutor::with_registries(registry, queue_manager))
            })
            .clone()
    }

    /// Parent session key derived from execution context.
    fn parent_session_key(ctx: Option<&ToolContext>) -> String {
        match ctx {
            Some(c) => format!(
                "{}_{}",
                c.agent_id.clone().unwrap_or_else(|| "unknown".to_string()),
                c.session_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string())
            ),
            None => "unknown".to_string(),
        }
    }

    /// Execute a shell command with an optional per-call timeout.
    async fn execute_command_blocking(
        command: &str,
        working_dir: Option<std::path::PathBuf>,
        timeout_ms: Option<u64>,
    ) -> Result<serde_json::Value> {
        let mut cmd = Command::new(SHELL);
        cmd.arg(SHELL_ARG).arg(command);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        let output = match timeout_ms {
            Some(ms) if ms > 0 => {
                tokio::time::timeout(tokio::time::Duration::from_millis(ms), cmd.output())
                    .await
                    .map_err(|_| anyhow::anyhow!("Bash command timed out after {ms} ms"))?
                    .context("Failed to execute Bash command")?
            }
            _ => cmd
                .output()
                .await
                .context("Failed to execute Bash command")?,
        };

        Self::format_output(&output)
    }

    /// Run a command in the background via the async executor.
    async fn execute_command_background(
        command: String,
        working_dir: Option<std::path::PathBuf>,
        timeout_ms: Option<u64>,
        parent_session_key: String,
    ) -> Result<serde_json::Value> {
        let task_id = format!("Bash:{}", uuid::Uuid::new_v4());
        let config = AsyncToolConfig {
            // Preserve millisecond precision by using `timeout_millis` when set;
            // the executor will fall back to `timeout_secs` otherwise.
            timeout_millis: timeout_ms,
            ..Default::default()
        };

        let receipt = Self::background_executor()
            .execute(
                task_id.clone(),
                "Bash",
                json!({ "command": &command, "cwd": working_dir.as_ref().map(|p| p.to_string_lossy().to_string()) }),
                parent_session_key,
                config,
                move || async move {
                    Self::execute_command_blocking(&command, working_dir, None).await
                },
            )
            .await?;

        Ok(json!({
            "task_id": receipt.task_id,
            "status": "running",
            "tool": "Bash",
        }))
    }

    /// Format command output
    fn format_output(output: &std::process::Output) -> Result<serde_json::Value> {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

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
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "success": output.status.success(),
        }))
    }

    /// Core execution dispatcher.
    async fn execute_with_maybe_context(
        &self,
        params: serde_json::Value,
        ctx: Option<&ToolContext>,
    ) -> Result<serde_json::Value> {
        let args: BashArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let cwd = self.resolve_cwd(args.cwd.as_deref());

        if args.run_in_background {
            Self::execute_command_background(
                args.command,
                cwd,
                args.timeout,
                Self::parent_session_key(ctx),
            )
            .await
        } else {
            Self::execute_command_blocking(&args.command, cwd, args.timeout).await
        }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "Bash"
    }

    fn description(&self) -> String {
        let (simple_cmd, pipe_cmd, redirect_cmd, env_cmd) = if cfg!(windows) {
            (
                r#"{"command": "Get-ChildItem"}"#,
                r#"{"command": "Get-Content file.txt | Select-String error | Select-Object -First 20"}"#,
                r#"{"command": "Write-Output 'hello' | Set-Content greeting.txt"}"#,
                r#"{"command": "Write-Output $env:USERPROFILE"}"#,
            )
        } else {
            (
                r#"{"command": "ls -la"}"#,
                r#"{"command": "cat file.txt | grep error | head -20"}"#,
                r#"{"command": "echo 'hello' > greeting.txt"}"#,
                r#"{"command": "echo $HOME"}"#,
            )
        };

        format!(
            r#"## Purpose
Execute system shell commands. Full shell access including pipes, redirection, and environment variables.

## Platform Information
- **OS**: {OS_DISPLAY}
- **Shell**: {SHELL_DISPLAY}

## Security Note
This tool has FULL SYSTEM ACCESS when enabled. It can:
- Execute any shell command
- Access all environment variables
- Read/write any file the OS user can access
- Run commands in any directory

Disable this tool in agent config if you don't need shell access.

## API
```json
{{
    "command": "your command here",
    "description": "what the command does",
    "cwd": "./subdir",
    "run_in_background": false,
    "timeout": 60000
}}
```

## Examples

Simple command:
```json
{simple_cmd}
```

With pipes:
```json
{pipe_cmd}
```

With redirection:
```json
{redirect_cmd}
```

Environment variables:
```json
{env_cmd}
```

Background execution:
```json
{{"command": "sleep 10 && echo done", "run_in_background": true}}
```

## Background-task lifecycle

When `run_in_background: true`, this tool returns a
`{{task_id, status: "running", tool: "Bash"}}` receipt immediately.
To monitor or cancel the backgrounded command, use the Async* family:

- `AsyncStatus({{task_id}})` — one-shot status (pending / running /
  completed / failed / cancelled / timed_out)
- `AsyncOutput({{task_id, block?, timeout?, tail_lines?}})` — read
  the result; with `block: true` the call waits until the task
  reaches a terminal state
- `AsyncStop({{task_id}})` — cancel a still-running task; returns
  `success: true, already_terminal: true` if the task is already done
- `AsyncList({{status_filter?, tool_filter?}})` — enumerate all
  background tasks visible to the current agent

The blocking form of this tool (default) is bounded only by the
`timeout` parameter; there is no implicit auto-detach to background.
"#
        )
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute (e.g., 'ls -la | grep foo')"
                },
                "description": {
                    "type": "string",
                    "description": "Optional human-readable description of the command"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command (default: agent workspace)"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "When true, run the command in the background and return a task receipt",
                    "default": false
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds for blocking execution",
                    "minimum": 1
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        self.execute_with_maybe_context(params, None).await
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value> {
        self.execute_with_maybe_context(params, Some(ctx)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn test_bash_tool_creation() {
        let tool = BashTool::new();
        assert_eq!(tool.name(), "Bash");
    }

    #[tokio::test]
    async fn test_bash_simple_command() {
        let tool = BashTool::new();

        let params = json!({"command": "echo hello"});

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {result:?}");

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        assert!(response["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_bash_with_pipes() {
        let tool = BashTool::new();

        let params = json!({
            "command": "echo -e 'line1\nline2\nline3' | grep line | wc -l"
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {:?}", result);

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        // Should output "3"
        assert!(response["stdout"].as_str().unwrap().trim() == "3");
    }

    #[tokio::test]
    async fn test_bash_with_cwd() {
        let temp_dir = TempDir::new().unwrap();
        let tool = BashTool::new().with_workspace(temp_dir.path());

        let test_file = temp_dir.path().join("test.txt");
        tokio::fs::write(&test_file, "test content").await.unwrap();

        let params = json!({
            "command": if cfg!(windows) { "type test.txt" } else { "cat test.txt" },
            "cwd": temp_dir.path().to_str().unwrap()
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {result:?}");

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        assert!(response["stdout"]
            .as_str()
            .unwrap()
            .contains("test content"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_bash_environment_access() {
        let tool = BashTool::new();

        let params = json!({"command": "echo $SHELL"});

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {:?}", result);

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let tool = BashTool::new();

        let sleep_cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 10"
        } else {
            "sleep 10"
        };

        let params = json!({"command": sleep_cmd, "timeout": 100});

        let result = tool.execute(params).await;
        assert!(
            result.is_err(),
            "Bash command should have timed out: {result:?}"
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_bash_nonexistent_command() {
        let tool = BashTool::new();

        let params = json!({"command": "this_command_definitely_does_not_exist_12345"});

        let result = tool.execute(params).await;
        assert!(
            result.is_ok(),
            "Should return result even for failed command"
        );

        let response = result.unwrap();
        assert!(!response["success"].as_bool().unwrap());
        assert_ne!(response["exit_code"].as_i64(), Some(0));
    }

    #[tokio::test]
    async fn test_bash_run_in_background_returns_receipt() {
        let tool = BashTool::new();

        let params = json!({
            "command": if cfg!(windows) { "Start-Sleep -Seconds 1; Write-Output done" } else { "sleep 1 && echo done" },
            "run_in_background": true,
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {result:?}");

        let response = result.unwrap();
        assert!(response["task_id"].as_str().unwrap().starts_with("Bash:"));
        assert_eq!(response["status"], "running");
        assert_eq!(response["tool"], "Bash");
    }
}
