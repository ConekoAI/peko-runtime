//! Process execution tool for running commands
//!
//! Implements CAPABILITY_INTERFACE.md §3.2
//! - Blocks shell execution (sh, bash, zsh, cmd, powershell)
//! - Strips sensitive env vars (*_API_KEY, *_SECRET, *_TOKEN, *_PASSWORD)
//! - Validates cwd is within sandbox
//! - Supports both sync and async execution modes

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use uuid::Uuid;

use crate::agent::async_tool_framework::{
    AsyncResultDeliveryMode, AsyncTaskResult, AsyncToolConfig, UnifiedAsyncExecutor,
};
use crate::tools::Tool;

/// List of blocked shell commands
const BLOCKED_SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "cmd", "powershell", "pwsh"];

/// List of sensitive env var patterns to strip
const SENSITIVE_ENV_PATTERNS: &[&str] = &[
    "_API_KEY",
    "_SECRET",
    "_SECRET_KEY",
    "_TOKEN",
    "_PASSWORD",
    "_KEY", // Generic key pattern
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "GITHUB_TOKEN",
];

/// Execution mode for process tool
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessMode {
    /// Synchronous: block until completion (default)
    Sync {
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
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
    120000 // 120 seconds default
}

/// Process tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessArgs {
    /// Command to execute (must not be a shell)
    pub command: String,
    /// Command arguments
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory (must be within sandbox)
    #[serde(default)]
    pub cwd: Option<String>,
    /// Additional environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Timeout in milliseconds
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Async mode
    #[serde(default)]
    pub r#async: Option<bool>,
    /// Stdin content
    #[serde(default)]
    pub stdin: Option<String>,
}

/// Process execution tool for running shell commands
pub struct ProcessTool {
    /// Default timeout in milliseconds
    default_timeout_ms: u64,
    /// Unified async executor (for async mode)
    executor: Option<UnifiedAsyncExecutor>,
    /// Parent session key (for async result routing)
    session_key: Option<String>,
    /// Workspace directory for cwd validation
    workspace_dir: Option<std::path::PathBuf>,
}

/// Maximum allowed timeout in milliseconds (5 minutes)
const MAX_TIMEOUT_MS: u64 = 300_000;
/// Default timeout in milliseconds (2 minutes)
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

impl ProcessTool {
    /// Create a new process tool with default settings
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

    /// Configure workspace directory for cwd validation
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

    /// Check if command is a blocked shell
    fn is_blocked_shell(&self, command: &str) -> bool {
        let cmd_lower = command.to_lowercase();
        let base_cmd = cmd_lower.split('/').last().unwrap_or(&cmd_lower);

        BLOCKED_SHELLS.contains(&base_cmd)
    }

    /// Validate and resolve cwd path
    fn validate_cwd(&self, cwd: &str) -> Result<std::path::PathBuf> {
        let path = std::path::PathBuf::from(cwd);

        // Check for path traversal
        if path.to_string_lossy().contains("..") {
            return Err(anyhow::anyhow!(
                "SandboxViolation: cwd contains path traversal: {}",
                cwd
            ));
        }

        // If we have a workspace directory, ensure cwd is within it
        if let Some(ref workspace) = self.workspace_dir {
            let resolved = if path.is_absolute() {
                path
            } else {
                workspace.join(&path)
            };

            // Use string comparison for consistency (avoids UNC prefix issues on Windows)
            let workspace_str = workspace.to_string_lossy();
            let resolved_str = resolved.to_string_lossy();

            if !resolved_str.starts_with(&*workspace_str) {
                return Err(anyhow::anyhow!(
                    "SandboxViolation: cwd {} is outside workspace {}",
                    resolved.display(),
                    workspace.display()
                ));
            }

            return Ok(resolved);
        }

        Ok(path)
    }

    /// Filter environment variables to remove sensitive ones
    fn filter_env_vars(&self, env: &HashMap<String, String>) -> HashMap<String, String> {
        let mut filtered = HashMap::new();

        for (key, value) in env {
            // Check if key matches any sensitive pattern
            let is_sensitive = SENSITIVE_ENV_PATTERNS.iter().any(|pattern| {
                key.to_uppercase().ends_with(pattern) || key.to_uppercase() == *pattern
            });

            if !is_sensitive {
                filtered.insert(key.clone(), value.clone());
            }
        }

        filtered
    }

    /// Get clean environment for child process
    fn get_clean_env(&self, extra_env: &HashMap<String, String>) -> HashMap<String, String> {
        // Start with minimal environment
        let mut env = HashMap::new();

        // Add only safe environment variables
        let safe_vars = ["PATH", "HOME", "USER", "LANG", "TERM"];
        for var in &safe_vars {
            if let Ok(value) = std::env::var(var) {
                env.insert(var.to_string(), value);
            }
        }

        // Add filtered extra env vars
        let filtered_extra = self.filter_env_vars(extra_env);
        env.extend(filtered_extra);

        env
    }

    /// Execute a command with arguments
    async fn execute_command(
        &self,
        command: &str,
        args: Vec<String>,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: &HashMap<String, String>,
        stdin: Option<&str>,
    ) -> Result<serde_json::Value> {
        // Validate command is not a shell
        if self.is_blocked_shell(command) {
            return Err(anyhow::anyhow!(
                "Shell execution is blocked. Use 'command' and 'args' parameters instead of shell syntax. "
            ));
        }

        // Validate and resolve working directory
        let cwd = if let Some(cwd) = working_dir {
            Some(self.validate_cwd(cwd)?)
        } else {
            self.workspace_dir.clone()
        };

        let mut cmd = Command::new(command);
        cmd.args(&args);

        // Set working directory
        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }

        // Set filtered environment
        let clean_env = self.get_clean_env(env_vars);
        cmd.env_clear();
        cmd.envs(&clean_env);

        // Set stdin if provided
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
                Ok(Err(e)) => Err(anyhow::anyhow!(
                    "Failed to execute command '{}': {}",
                    command,
                    e
                )),
                Err(_) => Err(anyhow::anyhow!(
                    "Command '{}' timed out after {} ms",
                    command,
                    timeout_ms
                )),
            };
        }

        // Execute with timeout
        let result = timeout(Duration::from_millis(timeout_ms), cmd.output()).await;

        match result {
            Ok(Ok(output)) => self.format_output(&output),
            Ok(Err(e)) => Err(anyhow::anyhow!(
                "Failed to execute command '{}': {}",
                command,
                e
            )),
            Err(_) => Err(anyhow::anyhow!(
                "Command '{}' timed out after {} ms",
                command,
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
        args: Vec<String>,
        timeout_ms: u64,
        working_dir: Option<String>,
        env_vars: HashMap<String, String>,
        stdin: Option<String>,
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

        // Validate command before spawning
        if self.is_blocked_shell(&command) {
            return Err(anyhow::anyhow!(
                "Shell execution is blocked. Use 'command' and 'args' parameters instead of shell syntax."
            ));
        }

        // Validate cwd
        let cwd = if let Some(ref cwd) = working_dir {
            Some(self.validate_cwd(cwd)?)
        } else {
            self.workspace_dir.clone()
        };

        // Clone values for the execution closure
        let workspace = self.workspace_dir.clone();

        // Clone command for the closure
        let command_for_closure = command.clone();

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
                    timeout_secs: timeout_ms / 1000,
                    cleanup_after_delivery: true,
                    label: label.clone(),
                },
                move || async move {
                    // Execute the command
                    let mut cmd = Command::new(&command_for_closure);
                    cmd.args(&args);

                    if let Some(ref dir) = cwd {
                        cmd.current_dir(dir);
                    }

                    // Set filtered environment
                    let clean_env = if let Some(ref ws) = workspace {
                        let tool = ProcessTool::new().with_workspace(ws.clone());
                        tool.get_clean_env(&env_vars)
                    } else {
                        env_vars
                    };
                    cmd.env_clear();
                    cmd.envs(&clean_env);

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
        "Execute system commands with sandboxing. Shell execution is blocked."
    }

    fn llm_description(&self) -> String {
        r#"## Purpose
Execute system commands with sync or async support. For build commands, git operations, system diagnostics, and long-running tasks.

## Security Restrictions
- **No shell execution**: Commands like `sh`, `bash`, `zsh`, `cmd`, `powershell` are blocked
- **Use `command` + `args`**: Instead of `sh -c "ls -la"`, use `command: "ls"`, `args: ["-la"]`
- **cwd sandboxing**: Working directory must be within the agent's workspace
- **Env var stripping**: Sensitive variables (*_API_KEY, *_SECRET, *_TOKEN, *_PASSWORD) are removed

## Modes

### Sync Mode (default)
Blocks until command completes. Use for quick commands.
```json
{"command": "git", "args": ["status"]}
```

### Async Mode
Returns receipt immediately. Command runs in background.
```json
{
    "command": "cargo",
    "args": ["build", "--release"],
    "async": true,
    "label": "release-build"
}
```

## Timeout
- Default: 120 seconds
- Max: 300 seconds (unless 0 to disable)
- 0 disables timeout for long downloads

## Examples

Good (compliant):
```json
{"command": "git", "args": ["log", "--oneline", "-10"]}
```

Bad (blocked - shell):
```json
{"command": "bash", "args": ["-c", "git status"]}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute (not a shell)"
                },
                "args": {
                    "type": "array",
                    "description": "Command arguments",
                    "items": { "type": "string" },
                    "default": []
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
                    "description": "Working directory (must be within workspace)"
                },
                "env": {
                    "type": "object",
                    "description": "Additional environment variables (sensitive vars will be stripped)",
                    "additionalProperties": { "type": "string" }
                },
                "async": {
                    "type": "boolean",
                    "description": "If true, return receipt and execute in background",
                    "default": false
                },
                "stdin": {
                    "type": "string",
                    "description": "Content to pipe to the process stdin"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: ProcessArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        // Validate command is not a shell
        if self.is_blocked_shell(&args.command) {
            return Err(anyhow::anyhow!(
                "Shell execution is blocked: '{}' is a shell. Use 'command' and 'args' parameters instead of shell syntax.",
                args.command
            ));
        }

        let timeout_ms = if args.timeout_ms == 0 {
            u64::MAX // Disable timeout
        } else {
            args.timeout_ms.min(MAX_TIMEOUT_MS)
        };

        // Determine execution mode
        let is_async = args.r#async.unwrap_or(false);

        if is_async {
            let label = None; // Could be extracted from params if needed
            let delivery_mode = AsyncResultDeliveryMode::default();

            self.execute_async(
                args.command,
                args.args,
                timeout_ms,
                args.cwd,
                args.env,
                args.stdin,
                label,
                delivery_mode,
            )
            .await
        } else {
            self.execute_command(
                &args.command,
                args.args,
                timeout_ms,
                args.cwd.as_deref(),
                &args.env,
                args.stdin.as_deref(),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::fs;

    #[test]
    fn test_process_tool_creation() {
        let tool = ProcessTool::new();
        assert_eq!(tool.name(), "process");
    }

    #[test]
    fn test_blocked_shells() {
        let tool = ProcessTool::new();

        // Blocked shells
        assert!(tool.is_blocked_shell("sh"));
        assert!(tool.is_blocked_shell("bash"));
        assert!(tool.is_blocked_shell("/bin/bash"));
        assert!(tool.is_blocked_shell("zsh"));
        assert!(tool.is_blocked_shell("cmd"));
        assert!(tool.is_blocked_shell("powershell"));
        assert!(tool.is_blocked_shell("pwsh"));

        // Allowed commands
        assert!(!tool.is_blocked_shell("git"));
        assert!(!tool.is_blocked_shell("ls"));
        assert!(!tool.is_blocked_shell("cargo"));
    }

    #[test]
    fn test_env_var_filtering() {
        let tool = ProcessTool::new();

        let mut env = HashMap::new();
        env.insert("NORMAL_VAR".to_string(), "value1".to_string());
        env.insert("ANTHROPIC_API_KEY".to_string(), "secret123".to_string());
        env.insert("MY_SECRET_KEY".to_string(), "secret456".to_string());
        env.insert("GITHUB_TOKEN".to_string(), "token789".to_string());
        env.insert("DB_PASSWORD".to_string(), "password".to_string());

        let filtered = tool.filter_env_vars(&env);

        assert!(filtered.contains_key("NORMAL_VAR"));
        assert!(!filtered.contains_key("ANTHROPIC_API_KEY"));
        assert!(!filtered.contains_key("MY_SECRET_KEY"));
        assert!(!filtered.contains_key("GITHUB_TOKEN"));
        assert!(!filtered.contains_key("DB_PASSWORD"));
    }

    #[test]
    fn test_cwd_validation() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ProcessTool::new().with_workspace(temp_dir.path());

        // Valid cwd within workspace (subdirectory that doesn't exist yet is allowed)
        let valid = tool.validate_cwd("subdir");
        assert!(valid.is_ok(), "Valid cwd failed: {:?}", valid);

        // Path traversal attempt
        let invalid = tool.validate_cwd("../etc");
        assert!(invalid.is_err());
        assert!(invalid
            .unwrap_err()
            .to_string()
            .contains("SandboxViolation"));
    }

    #[tokio::test]
    async fn test_process_blocked_shell() {
        let tool = ProcessTool::new();

        let params = json!({
            "command": "bash",
            "args": ["-c", "echo hello"]
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[tokio::test]
    async fn test_process_simple_command() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ProcessTool::new().with_workspace(temp_dir.path());

        // Use a cross-platform command: `whoami` on both Unix and Windows
        let params = json!({
            "command": "whoami"
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok(), "Failed: {:?}", result);

        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        // whoami outputs the current username - just verify we got some output
        assert!(!response["stdout"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_process_cwd() {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir(temp_dir.path().join("subdir"))
            .await
            .unwrap();

        let tool = ProcessTool::new().with_workspace(temp_dir.path());

        // Test that cwd validation passes for valid subdirectory
        // We can't easily test execution because 'echo' is shell builtin on Windows
        let valid = tool.validate_cwd("subdir");
        assert!(valid.is_ok(), "cwd validation should pass: {:?}", valid);

        // Verify the resolved path is correct
        let resolved = valid.unwrap();
        assert!(resolved.to_string_lossy().contains("subdir"));
    }

    #[test]
    fn test_env_var_filtering_comprehensive() {
        let tool = ProcessTool::new();

        let mut env = HashMap::new();
        // Should be allowed
        env.insert("NORMAL_VAR".to_string(), "value1".to_string());
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("HOME".to_string(), "/home/user".to_string());

        // Should be filtered - API keys
        env.insert("ANTHROPIC_API_KEY".to_string(), "sk-ant-12345".to_string());
        env.insert("OPENAI_API_KEY".to_string(), "sk-openai-12345".to_string());
        env.insert("MY_API_KEY".to_string(), "secret123".to_string());

        // Should be filtered - Secrets
        env.insert(
            "AWS_SECRET_ACCESS_KEY".to_string(),
            "aws-secret-123".to_string(),
        );
        env.insert("MY_SECRET".to_string(), "shhhh".to_string());
        env.insert("SECRET_KEY".to_string(), "another-secret".to_string());

        // Should be filtered - Tokens
        env.insert("GITHUB_TOKEN".to_string(), "ghp_12345".to_string());
        env.insert("AUTH_TOKEN".to_string(), "token123".to_string());
        env.insert("BEARER_TOKEN".to_string(), "bearer123".to_string());

        // Should be filtered - Passwords
        env.insert("DB_PASSWORD".to_string(), "password123".to_string());
        env.insert("ADMIN_PASSWORD".to_string(), "admin123".to_string());

        let filtered = tool.filter_env_vars(&env);

        // Allowed vars should be present
        assert!(filtered.contains_key("NORMAL_VAR"));
        assert!(filtered.contains_key("PATH"));
        assert!(filtered.contains_key("HOME"));

        // Sensitive vars should be filtered
        assert!(!filtered.contains_key("ANTHROPIC_API_KEY"));
        assert!(!filtered.contains_key("OPENAI_API_KEY"));
        assert!(!filtered.contains_key("MY_API_KEY"));
        assert!(!filtered.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!filtered.contains_key("MY_SECRET"));
        assert!(!filtered.contains_key("SECRET_KEY"));
        assert!(!filtered.contains_key("GITHUB_TOKEN"));
        assert!(!filtered.contains_key("AUTH_TOKEN"));
        assert!(!filtered.contains_key("BEARER_TOKEN"));
        assert!(!filtered.contains_key("DB_PASSWORD"));
        assert!(!filtered.contains_key("ADMIN_PASSWORD"));
    }

    #[test]
    fn test_blocked_shells_comprehensive() {
        let tool = ProcessTool::new();

        // All these should be blocked
        let blocked = vec![
            "sh",
            "bash",
            "zsh",
            "fish",
            "cmd",
            "powershell",
            "pwsh",
            "/bin/sh",
            "/bin/bash",
            "/bin/zsh",
            "/usr/bin/bash",
            "/usr/local/bin/zsh",
        ];

        for shell in &blocked {
            assert!(
                tool.is_blocked_shell(shell),
                "'{}' should be blocked as a shell",
                shell
            );
        }

        // These should be allowed
        let allowed = vec![
            "git",
            "cargo",
            "npm",
            "node",
            "python",
            "rustc",
            "ls",
            "cat",
            "grep",
            "find",
            "echo",
            "pwd",
            "docker",
            "kubectl",
            "terraform",
            "ansible",
        ];

        for cmd in &allowed {
            assert!(
                !tool.is_blocked_shell(cmd),
                "'{}' should not be blocked",
                cmd
            );
        }
    }

    #[test]
    fn test_cwd_path_traversal_blocked() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ProcessTool::new().with_workspace(temp_dir.path());

        // Path traversal attempts should be blocked
        let traversals = vec![
            "../etc",
            "../../etc/passwd",
            "..\\Windows\\System32",
            "foo/../../../etc/shadow",
            "./../..",
        ];

        for path in &traversals {
            let result = tool.validate_cwd(path);
            assert!(
                result.is_err(),
                "Path traversal '{}' should be blocked",
                path
            );
            let err = result.unwrap_err().to_string();
            assert!(err.contains("SandboxViolation"));
        }
    }

    #[test]
    fn test_cwd_absolute_outside_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ProcessTool::new().with_workspace(temp_dir.path());

        // Absolute paths outside workspace should be blocked
        #[cfg(unix)]
        let outside_paths = vec!["/etc", "/tmp", "/var/log", "/root"];

        #[cfg(windows)]
        let outside_paths = vec!["C:\\Windows", "D:\\", "C:\\Program Files"];

        for path in &outside_paths {
            let result = tool.validate_cwd(path);
            assert!(
                result.is_err(),
                "Absolute path '{}' outside workspace should be blocked",
                path
            );
        }
    }

    #[test]
    fn test_get_clean_env_strips_parent_env() {
        let tool = ProcessTool::new();

        // Set some parent environment variables that should be stripped
        std::env::set_var("TEST_API_KEY", "should-be-stripped");
        std::env::set_var("TEST_SECRET", "should-be-stripped");
        std::env::set_var("TEST_NORMAL", "should-be-allowed");

        let extra_env = HashMap::new();
        let clean_env = tool.get_clean_env(&extra_env);

        // API_KEY and SECRET patterns should be stripped even from parent
        assert!(!clean_env.contains_key("TEST_API_KEY"));
        assert!(!clean_env.contains_key("TEST_SECRET"));

        // But note: the filter only checks key names, not values
        // So TEST_NORMAL would be allowed if it were in the safe vars list
        // Actually, the function only adds specific safe vars from parent

        // Clean up
        std::env::remove_var("TEST_API_KEY");
        std::env::remove_var("TEST_SECRET");
        std::env::remove_var("TEST_NORMAL");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_process_shell_injection_blocked() {
        // This test demonstrates that shell injection is NOT possible
        // because we don't invoke a shell - args are passed literally to the command
        let tool = ProcessTool::new();

        // These would be dangerous if passed to a shell, but are safe here
        // because they're just string arguments to echo
        let test_cases = vec![
            ("echo", vec!["hello; rm -rf /".to_string()]),
            ("echo", vec!["hello && cat /etc/passwd".to_string()]),
            ("echo", vec!["hello || whoami".to_string()]),
            ("echo", vec!["$(rm -rf /)".to_string()]),
            ("echo", vec!["`whoami`".to_string()]),
        ];

        for (cmd, args) in &test_cases {
            // The command runs safely because args are passed literally, not interpreted by a shell
            let result = tool
                .execute_command(cmd, args.clone(), 5000, None, &HashMap::new(), None)
                .await;

            // Should succeed - the metacharacters are just part of the output string
            assert!(
                result.is_ok(),
                "Command should execute safely without shell interpretation"
            );

            // Verify the output contains the literal string (not executed)
            let output = result.unwrap();
            let stdout = output["stdout"].as_str().unwrap();
            assert!(
                stdout.contains("hello") || stdout.contains("rm") || stdout.contains("whoami"),
                "Output should contain the literal argument: {}",
                stdout
            );
        }
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn test_process_shell_injection_blocked_windows() {
        // Windows version of the test - uses whoami which is available on Windows
        let tool = ProcessTool::new();

        // Test that args with shell metacharacters are passed literally
        // whoami on Windows doesn't take args, but it won't execute the dangerous content
        let result = tool
            .execute_command(
                "whoami",
                vec!["test&whoami".to_string()], // This would be dangerous in cmd.exe
                5000,
                None,
                &HashMap::new(),
                None,
            )
            .await;

        // Should succeed - the & is just passed as an argument
        assert!(
            result.is_ok(),
            "Command should execute safely without shell interpretation"
        );
    }

    #[test]
    fn test_process_tool_with_timeout() {
        let tool = ProcessTool::with_timeout(60000); // 60 seconds
        assert_eq!(tool.default_timeout_ms, 60000);
    }
}
