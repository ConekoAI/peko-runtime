//! Shell execution tool for running system commands
//!
//! Implements ADR-014: All-or-nothing permission model
//! - Full shell access via system shell (sh/bash on Unix, cmd on Windows)
//! - No sandboxing, no command blocking, no env filtering
//! - Security boundary is tool enablement (enabled = full access)

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

fn default_timeout() -> u64 {
    120000 // 120 seconds default
}

/// Shell tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellArgs {
    /// Shell command to execute (passed directly to system shell)
    pub command: String,
    /// Working directory (defaults to workspace if set)
    #[serde(default)]
    pub cwd: Option<String>,
    /// Timeout in milliseconds (0 to disable, max: 300000)
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Execute asynchronously (returns receipt)
    #[serde(default)]
    pub r#async: Option<bool>,
    /// Stdin content to pipe to the command
    #[serde(default)]
    pub stdin: Option<String>,
}

/// Shell execution tool for running system commands
pub struct ShellTool {
    /// Default timeout in milliseconds
    default_timeout_ms: u64,
    /// Unified async executor (for async mode)
    executor: Option<UnifiedAsyncExecutor>,
    /// Parent session key (for async result routing)
    session_key: Option<String>,
    /// Workspace directory (default cwd)
    workspace_dir: Option<std::path::PathBuf>,
}

/// Maximum allowed timeout in milliseconds (5 minutes)
const MAX_TIMEOUT_MS: u64 = 300_000;
/// Default timeout in milliseconds (2 minutes)
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

impl ShellTool {
    /// Create a new shell tool with default settings
    #[must_use]
    pub fn new() -> Self {
        Self {
            default_timeout_ms: DEFAULT_TIMEOUT_MS,
            executor: None,
            session_key: None,
            workspace_dir: None,
        }
    }

    /// Create with custom timeout
    #[must_use]
    pub fn with_timeout(timeout_ms: u64) -> Self {
        Self {
            default_timeout_ms: timeout_ms.min(MAX_TIMEOUT_MS),
            executor: None,
            session_key: None,
            workspace_dir: None,
        }
    }

    /// Configure workspace directory (default working directory)
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<std::path::PathBuf>) -> Self {
        self.workspace_dir = Some(workspace.into());
        self
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

    /// Resolve working directory
    fn resolve_cwd(&self, cwd: Option<&str>) -> Option<std::path::PathBuf> {
        cwd.map(std::path::PathBuf::from)
            .or_else(|| self.workspace_dir.clone())
    }

    /// Execute a shell command
    async fn execute_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        stdin: Option<&str>,
    ) -> Result<serde_json::Value> {
        let cwd = self.resolve_cwd(working_dir);

        let mut cmd = Command::new(SHELL);
        cmd.arg(SHELL_ARG).arg(command);

        // Set working directory if specified
        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }

        // Handle stdin if provided
        if let Some(input) = stdin {
            use std::process::Stdio;
            use tokio::io::AsyncWriteExt;

            cmd.stdin(Stdio::piped());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());

            let mut child = cmd.spawn()?;

            if let Some(mut child_stdin) = child.stdin.take() {
                child_stdin.write_all(input.as_bytes()).await?;
            }

            let result = timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await;

            return match result {
                Ok(Ok(output)) => self.format_output(&output),
                Ok(Err(e)) => Err(anyhow::anyhow!("Failed to execute command: {}", e)),
                Err(_) => Err(anyhow::anyhow!(
                    "Command timed out after {} ms",
                    timeout_ms
                )),
            };
        }

        // Execute with timeout
        let result = timeout(Duration::from_millis(timeout_ms), cmd.output()).await;

        match result {
            Ok(Ok(output)) => self.format_output(&output),
            Ok(Err(e)) => Err(anyhow::anyhow!("Failed to execute command: {}", e)),
            Err(_) => Err(anyhow::anyhow!(
                "Command timed out after {} ms",
                timeout_ms
            )),
        }
    }

    /// Format command output
    fn format_output(&self, output: &std::process::Output) -> Result<serde_json::Value> {
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

    /// Execute command in async mode using UnifiedAsyncExecutor
    async fn execute_async(
        &self,
        command: String,
        timeout_ms: u64,
        working_dir: Option<String>,
        stdin: Option<String>,
    ) -> Result<serde_json::Value> {
        let executor = self
            .executor
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Async mode not configured for shell tool"))?;

        let session_key = self
            .session_key
            .clone()
            .unwrap_or_else(|| "default".to_string());

        let task_id = format!("shell_{}", Uuid::new_v4().simple());

        let cwd = self.resolve_cwd(working_dir.as_deref());

        // Clone command for the closure
        let command_for_closure = command.clone();

        // Execute using unified executor
        let receipt = executor
            .execute(
                task_id.clone(),
                "shell",
                json!({
                    "command": &command,
                    "working_dir": &working_dir,
                }),
                session_key,
                AsyncToolConfig {
                    delivery_mode: AsyncResultDeliveryMode::default(),
                    delivery_target: None,
                    timeout_secs: timeout_ms / 1000,
                    cleanup_after_delivery: true,
                    label: None,
                },
                move || async move {
                    let mut cmd = Command::new(SHELL);
                    cmd.arg(SHELL_ARG).arg(&command_for_closure);

                    if let Some(ref dir) = cwd {
                        cmd.current_dir(dir);
                    }

                    // Handle stdin
                    if let Some(input) = stdin {
                        use std::process::Stdio;
                        use tokio::io::AsyncWriteExt;

                        cmd.stdin(Stdio::piped());
                        cmd.stdout(Stdio::piped());
                        cmd.stderr(Stdio::piped());

                        let mut child = cmd
                            .spawn()
                            .map_err(|e| anyhow::anyhow!("Failed to spawn: {}", e))?;

                        if let Some(mut child_stdin) = child.stdin.take() {
                            child_stdin.write_all(input.as_bytes()).await?;
                        }

                        let output = child.wait_with_output().await?;
                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                        Ok(AsyncTaskResult::Process {
                            stdout,
                            stderr,
                            exit_code: output.status.code().unwrap_or(-1),
                        })
                    } else {
                        let output = cmd.output().await?;
                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                        Ok(AsyncTaskResult::Process {
                            stdout,
                            stderr,
                            exit_code: output.status.code().unwrap_or(-1),
                        })
                    }
                },
            )
            .await?;

        // Return receipt
        Ok(json!({
            "receipt_id": receipt.task_id,
            "status": "accepted",
            "mode": "async",
            "command": command,
            "check_status_tool": receipt.check_status_tool,
        }))
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn description(&self) -> &'static str {
        "Execute system shell commands with full access. Pipes, redirection, and shell builtins supported."
    }

    fn llm_description(&self) -> String {
        let (simple_cmd, pipe_cmd, redirect_cmd, env_cmd, async_cmd) = if cfg!(windows) {
            (
                r#"{"command": "Get-ChildItem"}"#,
                r#"{"command": "Get-Content file.txt | Select-String error | Select-Object -First 20"}"#,
                r#"{"command": "Write-Output 'hello' | Set-Content greeting.txt"}"#,
                r#"{"command": "Write-Output $env:USERPROFILE"}"#,
                r#"{"command": ".\\long-build-script.ps1", "async": true, "timeout_ms": 300000}"#,
            )
        } else {
            (
                r#"{"command": "ls -la"}"#,
                r#"{"command": "cat file.txt | grep error | head -20"}"#,
                r#"{"command": "echo 'hello' > greeting.txt"}"#,
                r#"{"command": "echo $HOME"}"#,
                r#"{"command": "./long-build-script.sh", "async": true, "timeout_ms": 300000}"#,
            )
        };

        format!(
            r#"## Purpose
Execute system shell commands. Full shell access including pipes, redirection, and environment variables.

## Platform Information
- **OS**: {os}
- **Shell**: {shell}

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
    "timeout_ms": 30000,
    "async": false,
    "cwd": "./subdir"
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

Async execution:
```json
{async_cmd}
```"#,
            os = OS_DISPLAY,
            shell = SHELL_DISPLAY,
            simple_cmd = simple_cmd,
            pipe_cmd = pipe_cmd,
            redirect_cmd = redirect_cmd,
            env_cmd = env_cmd,
            async_cmd = async_cmd
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
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default: 120000). Set to 0 to disable. Max: 300000",
                    "minimum": 0,
                    "maximum": 300000,
                    "default": 120000
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command (default: agent workspace)"
                },
                "async": {
                    "type": "boolean",
                    "description": "If true, return receipt and execute in background",
                    "default": false
                },
                "stdin": {
                    "type": "string",
                    "description": "Content to pipe to the command's stdin"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: ShellArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        let timeout_ms = if args.timeout_ms == 0 {
            u64::MAX // Disable timeout
        } else {
            args.timeout_ms.min(MAX_TIMEOUT_MS)
        };

        // Determine execution mode
        let is_async = args.r#async.unwrap_or(false);

        if is_async {
            self.execute_async(args.command, timeout_ms, args.cwd, args.stdin)
                .await
        } else {
            self.execute_command(&args.command, timeout_ms, args.cwd.as_deref(), args.stdin.as_deref())
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn test_shell_tool_creation() {
        let tool = ShellTool::new();
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn test_shell_tool_with_timeout() {
        let tool = ShellTool::with_timeout(60000); // 60 seconds
        assert_eq!(tool.default_timeout_ms, 60000);
    }

    #[tokio::test]
    async fn test_shell_simple_command() {
        let tool = ShellTool::new();

        // Use a cross-platform command
        let params = json!({
            "command": if cfg!(windows) { "echo hello" } else { "echo hello" }
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {:?}", result);

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        assert!(response["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_with_pipes() {
        let tool = ShellTool::new();

        let params = json!({
            "command": "echo -e 'line1\\nline2\\nline3' | grep line | wc -l"
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {:?}", result);

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        // Should output "3"
        assert!(response["stdout"].as_str().unwrap().trim() == "3");
    }

    #[tokio::test]
    async fn test_shell_with_cwd() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ShellTool::new().with_workspace(temp_dir.path());

        // Create a file in temp directory
        let test_file = temp_dir.path().join("test.txt");
        tokio::fs::write(&test_file, "test content")
            .await
            .unwrap();

        let params = json!({
            "command": if cfg!(windows) { "type test.txt" } else { "cat test.txt" },
            "cwd": temp_dir.path().to_str().unwrap()
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {:?}", result);

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        assert!(response["stdout"].as_str().unwrap().contains("test content"));
    }

    #[tokio::test]
    async fn test_shell_timeout() {
        let tool = ShellTool::new();

        // Use a command that sleeps longer than timeout
        // On Windows: ping -n 6 127.0.0.1 takes ~5 seconds
        // On Unix: sleep 5 takes 5 seconds
        let params = json!({
            "command": if cfg!(windows) { "ping -n 6 127.0.0.1" } else { "sleep 5" },
            "timeout_ms": 100
        });

        let result = tool.execute(params).await;
        assert!(result.is_err(), "Expected timeout error, got: {:?}", result);
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_environment_access() {
        let tool = ShellTool::new();

        // Test that we can access environment variables
        let params = json!({
            "command": "echo $SHELL"
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {:?}", result);

        // Just verify it runs without error
        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_shell_nonexistent_command() {
        let tool = ShellTool::new();

        let params = json!({
            "command": "this_command_definitely_does_not_exist_12345"
        });

        let result = tool.execute(params).await;
        // Should succeed in execution but with non-zero exit code
        assert!(result.is_ok(), "Should return result even for failed command");

        let response = result.unwrap();
        assert!(!response["success"].as_bool().unwrap());
        assert_ne!(response["exit_code"].as_i64(), Some(0));
    }

    #[tokio::test]
    async fn test_shell_stdin() {
        let tool = ShellTool::new();

        let params = json!({
            "command": if cfg!(windows) { "Read-Host" } else { "cat" },
            "stdin": "hello from stdin"
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {:?}", result);

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        assert!(response["stdout"]
            .as_str()
            .unwrap()
            .contains("hello from stdin"));
    }
}
