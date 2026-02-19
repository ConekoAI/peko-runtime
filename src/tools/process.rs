//! Process execution tool for running commands

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::tools::Tool;

/// Process execution tool for running shell commands
pub struct ProcessTool {
    /// Default timeout in seconds
    default_timeout_secs: u64,
    /// Whether to allow shell commands (sh -c)
    allow_shell: bool,
}

impl ProcessTool {
    /// Create a new process tool with default settings
    #[must_use] 
    pub fn new() -> Self {
        Self {
            default_timeout_secs: 30,
            allow_shell: false,
        }
    }

    /// Create with custom timeout
    #[must_use] 
    pub fn with_timeout(timeout_secs: u64) -> Self {
        Self {
            default_timeout_secs: timeout_secs,
            allow_shell: false,
        }
    }

    /// Create with shell support enabled
    #[must_use] 
    pub fn with_shell() -> Self {
        Self {
            default_timeout_secs: 30,
            allow_shell: true,
        }
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
        "Execute system commands. Parameters: {\"command\": string, \"args\": [string] (optional), \"timeout\": number (seconds, optional), \"working_dir\": string (optional), \"env\": object (optional)}"
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
            .unwrap_or(self.default_timeout_secs);

        let working_dir = params.get("working_dir").and_then(|v| v.as_str());

        let env_vars = params.get("env").and_then(|v| v.as_object()).cloned();

        Self::execute_command(command, args, timeout_secs, working_dir, env_vars).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_process_tool_creation() {
        let tool = ProcessTool::new();
        assert_eq!(tool.name(), "process");
        assert!(!tool.description().is_empty());
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
