//! Shell execution tool for running system commands
//!
//! Implements ADR-014: All-or-nothing permission model
//! - Full shell access via system shell (sh/bash on Unix, cmd on Windows)
//! - No sandboxing, no command blocking, no env filtering
//! - Security boundary is tool enablement (enabled = full access)
//!
//! Note: Async execution and timeout are handled by the framework-level
//! `ToolWrapper` using `_async` and `_timeout` parameters.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;

use crate::tools::core::Tool;

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

/// Shell tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellArgs {
    /// Shell command to execute (passed directly to system shell)
    pub command: String,
    /// Working directory (defaults to workspace if set)
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Shell execution tool for running system commands
pub struct ShellTool {
    /// Workspace directory (default cwd)
    workspace_dir: Option<std::path::PathBuf>,
}

impl ShellTool {
    /// Create a new shell tool with default settings
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

    /// Execute a shell command
    async fn execute_command(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Result<serde_json::Value> {
        let cwd = self.resolve_cwd(working_dir);

        let mut cmd = Command::new(SHELL);
        cmd.arg(SHELL_ARG).arg(command);

        // Set working directory if specified
        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }

        // Execute command
        let output = cmd.output().await?;
        self.format_output(&output)
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

## Async Execution

For long-running commands, use the framework-level async parameter:
```json
{{"command": "./long-build-script.sh", "_async": true, "_timeout": 300}}
```"#
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
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command (default: agent workspace)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: ShellArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        self.execute_command(&args.command, args.cwd.as_deref())
            .await
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

    #[tokio::test]
    async fn test_shell_simple_command() {
        let tool = ShellTool::new();

        // Use a cross-platform command
        let params = json!({
            "command": if cfg!(windows) { "echo hello" } else { "echo hello" }
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {result:?}");

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        assert!(response["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_with_pipes() {
        let tool = ShellTool::new();

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
    async fn test_shell_with_cwd() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ShellTool::new().with_workspace(temp_dir.path());

        // Create a file in temp directory
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
    async fn test_shell_timeout() {
        let tool = ShellTool::new();

        // Use a cross-platform sleep command
        let sleep_cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 10"
        } else {
            "sleep 10"
        };

        let params = json!({"command": sleep_cmd});

        let result =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), tool.execute(params)).await;

        assert!(
            result.is_err(),
            "Shell command should have timed out, but it completed: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_shell_nonexistent_command() {
        let tool = ShellTool::new();

        let params = json!({
            "command": "this_command_definitely_does_not_exist_12345"
        });

        let result = tool.execute(params).await;
        // Should succeed in execution but with non-zero exit code
        assert!(
            result.is_ok(),
            "Should return result even for failed command"
        );

        let response = result.unwrap();
        assert!(!response["success"].as_bool().unwrap());
        assert_ne!(response["exit_code"].as_i64(), Some(0));
    }
}
