//! Custom tool protocol - JSON over stdin/stdout
//!
//! Implements the protocol defined in DATA_MODEL.md §10:
//! - Request: JSON object written to stdin, followed by newline
//! - Response: JSON object written to stdout, followed by newline
//! - Exit codes: 0 = success, 1 = tool error, 2 = protocol error

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, error, trace, warn};

/// Request sent to custom tool via stdin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRequest {
    /// Unique tool call ID
    pub tool_call_id: String,
    /// Tool name
    pub tool: String,
    /// Tool arguments
    pub args: serde_json::Value,
    /// Timeout in milliseconds
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Execution context
    #[serde(default)]
    pub context: ExecutionContext,
}

fn default_timeout() -> u64 {
    30000 // 30 seconds default
}

/// Execution context passed to custom tools
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionContext {
    /// Instance ID
    pub instance_id: String,
    /// Session ID
    pub session_id: String,
    /// Workspace directory path
    pub workspace: PathBuf,
}

/// Response from custom tool via stdout
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResponse {
    /// Tool call ID (matches request)
    pub tool_call_id: String,
    /// Output data (if success)
    #[serde(default)]
    pub output: Option<String>,
    /// Error message (if failure)
    #[serde(default)]
    pub error: Option<String>,
}

impl ToolResponse {
    /// Create a success response
    pub fn success(tool_call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            output: Some(output.into()),
            error: None,
        }
    }

    /// Create an error response
    pub fn error(tool_call_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            output: None,
            error: Some(error.into()),
        }
    }

    /// Check if this is a success response
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Convert to JSON value
    pub fn to_json(&self) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::to_value(self)?)
    }
}

/// Exit codes for custom tools
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Success (0)
    Success,
    /// Tool error (1)
    ToolError,
    /// Protocol error (2)
    ProtocolError,
    /// Unknown/other
    Unknown(i32),
}

impl From<i32> for ExitCode {
    fn from(code: i32) -> Self {
        match code {
            0 => ExitCode::Success,
            1 => ExitCode::ToolError,
            2 => ExitCode::ProtocolError,
            n => ExitCode::Unknown(n),
        }
    }
}

/// Execute a custom tool with the JSON protocol
///
/// # Arguments
/// * `executable` - Path to the tool executable
/// * `request` - The tool request
/// * `timeout` - Maximum execution time
///
/// # Returns
/// * `Ok(ToolResponse)` - Tool executed and returned valid response
/// * `Err(...)` - Execution failed (timeout, protocol error, etc.)
pub async fn execute_tool(
    executable: impl AsRef<std::path::Path>,
    request: &ToolRequest,
    timeout: Duration,
) -> anyhow::Result<ToolResponse> {
    let executable = executable.as_ref();
    let tool_name = &request.tool;
    let tool_call_id = &request.tool_call_id;

    trace!(
        "Executing custom tool '{}' (call_id: {}) at {:?}",
        tool_name,
        tool_call_id,
        executable
    );

    // Serialize request
    let request_json = serde_json::to_string(request)?;
    debug!(
        "Tool request ({}): {}",
        tool_call_id,
        request_json.len() // Don't log full content in case of sensitive data
    );

    // Spawn the tool process
    let mut child = Command::new(executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn tool '{}': {}", tool_name, e))?;

    // Write request to stdin
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to open stdin for tool '{}'", tool_name))?;

    let request_json_clone = request_json.clone();
    let stdin_task = tokio::spawn(async move {
        let mut stdin = stdin;
        stdin.write_all(request_json_clone.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok::<(), std::io::Error>(())
    });

    // Wait for stdin write to complete
    if let Err(e) = stdin_task.await {
        return Err(anyhow::anyhow!("Failed to write to tool stdin: {}", e));
    }

    // Read response from stdout with timeout
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to open stdout for tool '{}'", tool_name))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to open stderr for tool '{}'", tool_name))?;

    // Read stdout and stderr concurrently
    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        lines.next_line().await
    });

    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let mut stderr_output = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            stderr_output.push_str(&line);
            stderr_output.push('\n');
        }
        stderr_output
    });

    // Wait for the process with timeout
    let result = tokio::time::timeout(timeout, child.wait()).await;

    let exit_status = match result {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            return Err(anyhow::anyhow!("Tool process error: {}", e));
        }
        Err(_) => {
            // Timeout - kill the process
            warn!("Tool '{}' timed out after {:?}", tool_name, timeout);
            let _ = child.start_kill();
            return Err(anyhow::anyhow!(
                "Tool '{}' timed out after {}s",
                tool_name,
                timeout.as_secs()
            ));
        }
    };

    // Get stdout response
    let stdout_result = match stdout_task.await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => {
            return Err(anyhow::anyhow!("Tool '{}' returned no output", tool_name));
        }
        Ok(Err(e)) => {
            return Err(anyhow::anyhow!("Failed to read tool stdout: {}", e));
        }
        Err(e) => {
            return Err(anyhow::anyhow!("Stdout task panicked: {}", e));
        }
    };

    // Get stderr (for logging)
    let stderr_output = match stderr_task.await {
        Ok(output) => output,
        Err(_) => String::new(),
    };

    // Log stderr if there's content
    if !stderr_output.is_empty() {
        debug!("Tool '{}' stderr: {}", tool_name, stderr_output);
    }

    // Parse response
    trace!("Tool response ({}): {}", tool_call_id, stdout_result);

    let response: ToolResponse = match serde_json::from_str(&stdout_result) {
        Ok(r) => r,
        Err(e) => {
            // Invalid JSON response - treat as protocol error
            error!(
                "Tool '{}' returned invalid JSON: {}. Output: {}",
                tool_name, e, stdout_result
            );
            return Err(anyhow::anyhow!(
                "Tool '{}' protocol error: invalid JSON response",
                tool_name
            ));
        }
    };

    // Validate tool_call_id matches
    if response.tool_call_id != *tool_call_id {
        warn!(
            "Tool '{}' response ID mismatch: expected {}, got {}",
            tool_name, tool_call_id, response.tool_call_id
        );
        // Continue anyway - this is a warning, not fatal
    }

    // Handle exit code
    let exit_code = exit_status.code().unwrap_or(-1);
    match ExitCode::from(exit_code) {
        ExitCode::Success => {
            debug!("Tool '{}' completed successfully", tool_name);
            Ok(response)
        }
        ExitCode::ToolError => {
            debug!("Tool '{}' returned error: {:?}", tool_name, response.error);
            Ok(response) // Return the error response to the caller
        }
        ExitCode::ProtocolError => {
            error!("Tool '{}' returned protocol error (exit code 2)", tool_name);
            Err(anyhow::anyhow!(
                "Tool '{}' protocol error (exit code 2)",
                tool_name
            ))
        }
        ExitCode::Unknown(code) => {
            warn!(
                "Tool '{}' returned unexpected exit code: {}",
                tool_name, code
            );
            Ok(response) // Still try to return the response
        }
    }
}

/// Execute a tool with string arguments (convenience method)
pub async fn execute_tool_simple(
    executable: impl AsRef<std::path::Path>,
    tool_name: impl Into<String>,
    tool_call_id: impl Into<String>,
    args: serde_json::Value,
    context: ExecutionContext,
    timeout: Option<Duration>,
) -> anyhow::Result<ToolResponse> {
    let request = ToolRequest {
        tool_call_id: tool_call_id.into(),
        tool: tool_name.into(),
        args,
        timeout_ms: timeout
            .map(|d| d.as_millis() as u64)
            .unwrap_or_else(default_timeout),
        context,
    };

    execute_tool(
        executable,
        &request,
        timeout.unwrap_or(Duration::from_secs(30)),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn test_tool_request_serialization() {
        let request = ToolRequest {
            tool_call_id: "tc_123".to_string(),
            tool: "my_tool".to_string(),
            args: serde_json::json!({"query": "test"}),
            timeout_ms: 30000,
            context: ExecutionContext {
                instance_id: "inst_456".to_string(),
                session_id: "sess_789".to_string(),
                workspace: PathBuf::from("/workspace"),
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("tc_123"));
        assert!(json.contains("my_tool"));
        assert!(json.contains("query"));
    }

    #[test]
    fn test_tool_request_default_timeout() {
        let request = ToolRequest {
            tool_call_id: "tc_123".to_string(),
            tool: "my_tool".to_string(),
            args: serde_json::json!({}),
            timeout_ms: default_timeout(),
            context: ExecutionContext::default(),
        };

        assert_eq!(request.timeout_ms, 30000);
    }

    #[test]
    fn test_tool_response_serialization() {
        let response = ToolResponse::success("tc_123", "result data");
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("tc_123"));
        assert!(json.contains("result data"));
        assert!(json.contains("output"));
    }

    #[test]
    fn test_tool_response_error() {
        let response = ToolResponse::error("tc_456", "something went wrong");

        assert!(!response.is_success());
        assert_eq!(response.tool_call_id, "tc_456");
        assert_eq!(response.error, Some("something went wrong".to_string()));
        assert!(response.output.is_none());
    }

    #[test]
    fn test_tool_response_success() {
        let response = ToolResponse::success("tc_789", "great success");

        assert!(response.is_success());
        assert_eq!(response.tool_call_id, "tc_789");
        assert_eq!(response.output, Some("great success".to_string()));
        assert!(response.error.is_none());
    }

    #[test]
    fn test_tool_response_to_json() {
        let response = ToolResponse::success("tc_123", "data");
        let json = response.to_json().unwrap();

        assert!(json.get("tool_call_id").is_some());
        assert!(json.get("output").is_some());
        assert!(json.get("error").is_some());
    }

    #[test]
    fn test_exit_code_from_i32() {
        assert_eq!(ExitCode::from(0), ExitCode::Success);
        assert_eq!(ExitCode::from(1), ExitCode::ToolError);
        assert_eq!(ExitCode::from(2), ExitCode::ProtocolError);
        assert_eq!(ExitCode::from(42), ExitCode::Unknown(42));
        assert_eq!(ExitCode::from(-1), ExitCode::Unknown(-1));
    }

    #[test]
    fn test_execution_context_default() {
        let ctx = ExecutionContext::default();
        assert!(ctx.instance_id.is_empty());
        assert!(ctx.session_id.is_empty());
        assert!(ctx.workspace.as_os_str().is_empty());
    }

    #[tokio::test]
    async fn test_execute_tool_nonexistent() {
        let request = ToolRequest {
            tool_call_id: "tc_test".to_string(),
            tool: "nonexistent".to_string(),
            args: serde_json::json!({}),
            timeout_ms: 30000,
            context: ExecutionContext::default(),
        };

        let result: anyhow::Result<ToolResponse> = execute_tool(
            Path::new("/nonexistent/tool"),
            &request,
            Duration::from_secs(1),
        )
        .await;

        // Should fail because tool doesn't exist
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_execute_tool_with_timeout() {
        let temp_dir = TempDir::new().unwrap();

        // Create a tool that sleeps (will timeout)
        let tool_path = temp_dir.path().join("slow_tool.sh");
        let script = r#"#!/bin/bash
read line
sleep 10
"#;
        std::fs::write(&tool_path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tool_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tool_path, perms).unwrap();

        let request = ToolRequest {
            tool_call_id: "tc_test".to_string(),
            tool: "slow".to_string(),
            args: serde_json::json!({}),
            timeout_ms: 30000,
            context: ExecutionContext::default(),
        };

        let result = execute_tool(&tool_path, &request, Duration::from_millis(100)).await;

        // Should timeout
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_execute_tool_simple_convenience() {
        // This test verifies the convenience function exists and accepts parameters
        // Actual execution is tested separately
        let _temp_dir = TempDir::new().unwrap();

        // Just verify the function signature compiles
        let _future = execute_tool_simple(
            Path::new("/fake/tool"),
            "test_tool",
            "tc_123",
            serde_json::json!({"key": "value"}),
            ExecutionContext::default(),
            Some(Duration::from_secs(5)),
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_execute_tool_invalid_json_response() {
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();

        // Create a tool that returns invalid JSON
        let tool_path = temp_dir.path().join("bad_tool.sh");
        let script = r#"#!/bin/bash
read line
echo "not valid json"
"#;
        std::fs::write(&tool_path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tool_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tool_path, perms).unwrap();

        let request = ToolRequest {
            tool_call_id: "tc_test".to_string(),
            tool: "bad".to_string(),
            args: serde_json::json!({}),
            timeout_ms: 30000,
            context: ExecutionContext::default(),
        };

        let result = execute_tool(&tool_path, &request, Duration::from_secs(1)).await;

        // Should fail due to invalid JSON
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_execute_tool_exit_code_1() {
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();

        // Create a tool that exits with code 1 but returns valid JSON
        let tool_path = temp_dir.path().join("error_tool.sh");
        let script = r#"#!/bin/bash
read line
echo '{"tool_call_id": "tc_test", "output": null, "error": "tool failed"}'
exit 1
"#;
        std::fs::write(&tool_path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tool_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tool_path, perms).unwrap();

        let request = ToolRequest {
            tool_call_id: "tc_test".to_string(),
            tool: "error".to_string(),
            args: serde_json::json!({}),
            timeout_ms: 30000,
            context: ExecutionContext::default(),
        };

        let result = execute_tool(&tool_path, &request, Duration::from_secs(1)).await;

        // Should succeed (exit code 1 is tool error, not protocol error)
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(!response.is_success());
    }

    #[tokio::test]
    async fn test_execute_tool_exit_code_2() {
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();

        // Create a tool that exits with code 2 (protocol error)
        #[cfg(unix)]
        {
            let tool_path = temp_dir.path().join("protocol_error.sh");
            let script = r#"#!/bin/bash
read line
exit 2
"#;
            std::fs::write(&tool_path, script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&tool_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&tool_path, perms).unwrap();
        }

        let request = ToolRequest {
            tool_call_id: "tc_test".to_string(),
            tool: "protocol_error".to_string(),
            args: serde_json::json!({}),
            timeout_ms: 30000,
            context: ExecutionContext::default(),
        };

        let tool_path = temp_dir.path().join("protocol_error.sh");
        let result = execute_tool(&tool_path, &request, Duration::from_secs(1)).await;

        // Should fail (exit code 2 is protocol error)
        assert!(result.is_err());
    }
}
