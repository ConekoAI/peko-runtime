//! Universal Tool Transport - Stdio communication
//!
//! SRP: This module ONLY handles message transport over stdio.
//! No protocol parsing, no execution logic.

use super::protocol::{Request, Response};
use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

/// Transport handle for communicating with a tool process
pub struct Transport {
    stdin: tokio::process::ChildStdin,
    stdout_reader: BufReader<tokio::process::ChildStdout>,
    stderr_reader: Option<BufReader<tokio::process::ChildStderr>>,
    _child: Child, // Keep child alive
}

impl Transport {
    /// Spawn a tool and create transport
    /// 
    /// Automatically detects script files (.py, .js) and uses appropriate interpreter
    pub async fn spawn(executable: impl AsRef<std::path::Path>) -> Result<Self> {
        let executable = executable.as_ref();
        let extension = executable.extension().and_then(|e| e.to_str()).unwrap_or("");
        
        // Determine command and arguments based on file extension
        let (cmd, args): (String, Vec<String>) = match extension {
            "py" => {
                // Python script - use python/python3
                let python_cmd = if cfg!(windows) { "python" } else { "python3" };
                (python_cmd.to_string(), vec![executable.to_string_lossy().to_string()])
            }
            "js" => {
                // Node.js script - use node
                ("node".to_string(), vec![executable.to_string_lossy().to_string()])
            }
            _ => {
                // Binary executable - run directly
                (executable.to_string_lossy().to_string(), vec![])
            }
        };

        let mut command = Command::new(&cmd);
        if !args.is_empty() {
            command.args(&args);
        }
        
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn tool: {:?} (cmd: {}, args: {:?})", executable, cmd, args))?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to open stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to open stdout")?;
        let stderr = child.stderr.take();

        Ok(Self {
            stdin,
            stdout_reader: BufReader::new(stdout),
            stderr_reader: stderr.map(BufReader::new),
            _child: child,
        })
    }

    /// Send a request and wait for response
    pub async fn request(&mut self, req: &Request, timeout_secs: u64) -> Result<Response> {
        let req_json = serde_json::to_string(req)?;

        // Send request
        self.send_line(&req_json).await?;

        // Read response with timeout
        let response_json = timeout(
            Duration::from_secs(timeout_secs),
            self.read_line(),
        )
        .await
        .context("Tool request timed out")??;

        // Parse response
        let response: Response = serde_json::from_str(&response_json)
            .with_context(|| format!("Invalid JSON response: {}", response_json))?;

        // Verify id matches
        if response.id != req.id {
            return Err(anyhow::anyhow!(
                "Response ID mismatch: expected {:?}, got {:?}",
                req.id,
                response.id
            ));
        }

        Ok(response)
    }

    /// Send a notification (fire and forget)
    pub async fn notify(&mut self, req: &Request) -> Result<()> {
        let req_json = serde_json::to_string(req)?;
        self.send_line(&req_json).await
    }

    /// Send a line (with newline)
    async fn send_line(&mut self, line: &str) -> Result<()> {
        self.stdin
            .write_all(line.as_bytes())
            .await
            .context("Failed to write to tool stdin")?;
        self.stdin
            .write_all(b"\n")
            .await
            .context("Failed to write newline")?;
        self.stdin.flush().await.context("Failed to flush stdin")?;
        Ok(())
    }

    /// Read a line from stdout
    async fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        self.stdout_reader
            .read_line(&mut line)
            .await
            .context("Failed to read from tool stdout")?;

        if line.is_empty() {
            return Err(anyhow::anyhow!("Tool closed stdout (EOF)"));
        }

        // Trim trailing newline
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }

        Ok(line)
    }

    /// Start stderr reader (optional)
    pub fn start_stderr_reader(&mut self) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel(100);

        if let Some(mut reader) = self.stderr_reader.take() {
            tokio::spawn(async move {
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break, // EOF
                        Ok(_) => {
                            let trimmed = line.trim().to_string();
                            if tx.send(trimmed).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        rx
    }
}

/// Simple transport for testing
#[cfg(test)]
pub struct MockTransport {
    responses: Vec<Response>,
    current: usize,
}

#[cfg(test)]
impl MockTransport {
    pub fn new(responses: Vec<Response>) -> Self {
        Self { responses, current: 0 }
    }

    pub async fn request(&mut self, _req: &Request) -> Result<Response> {
        if self.current >= self.responses.len() {
            return Err(anyhow::anyhow!("No more mock responses"));
        }
        let resp = self.responses[self.current].clone();
        self.current += 1;
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::protocol::{ErrorObject, ResponseResult};

    #[tokio::test]
    async fn test_mock_transport() {
        let responses = vec![Response {
            jsonrpc: "2.0".to_string(),
            id: Some("1".to_string()),
            result: ResponseResult::Result(serde_json::json!({"success": true})),
        }];

        let mut transport = MockTransport::new(responses);
        let req = Request::new("test", serde_json::json!({}));
        let resp = transport.request(&req).await.unwrap();

        match resp.result {
            ResponseResult::Result(v) => assert_eq!(v["success"], true),
            _ => panic!("Expected success"),
        }
    }

    #[test]
    fn test_error_object() {
        let err = ErrorObject::internal_error("something broke");
        assert_eq!(err.code, -32603);
        assert!(err.message.contains("broke"));
    }
}
