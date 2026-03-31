//! Shared Process Transport
//!
//! Provides a unified interface for spawning and managing child processes
//! for both Universal Tools and MCP servers. This eliminates duplication
//! between the two transport implementations.
//!
//! # Architecture
//!
//! - `ProcessTransport`: Low-level process management (spawn, kill, wait)
//! - `ProcessBuilder`: Builder pattern for configuring process spawning
//! - `ProcessConfig`: Configuration for interpreter detection and environment

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;
use tracing::{debug, error, trace, warn};

/// Default timeout for graceful process shutdown
const DEFAULT_KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Configuration for process spawning
#[derive(Debug, Clone)]
pub struct ProcessConfig {
    /// Environment variables to set
    pub env: HashMap<String, String>,
    /// Working directory
    pub cwd: Option<PathBuf>,
    /// Enable stderr logging
    pub log_stderr: bool,
    /// Kill timeout for graceful shutdown
    pub kill_timeout: Duration,
    /// Auto-detect interpreter for script files (.py, .js)
    pub auto_interpreter: bool,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            env: HashMap::new(),
            cwd: None,
            log_stderr: true,
            kill_timeout: DEFAULT_KILL_TIMEOUT,
            auto_interpreter: true,
        }
    }
}

impl ProcessConfig {
    /// Create a new config with defaults
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an environment variable
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set working directory
    pub fn cwd(mut self, path: impl AsRef<Path>) -> Self {
        self.cwd = Some(path.as_ref().to_path_buf());
        self
    }

    /// Enable/disable stderr logging
    pub fn log_stderr(mut self, enabled: bool) -> Self {
        self.log_stderr = enabled;
        self
    }

    /// Set kill timeout
    pub fn kill_timeout(mut self, secs: u64) -> Self {
        self.kill_timeout = Duration::from_secs(secs);
        self
    }
}

/// Resolved command information after interpreter detection
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    /// The command to execute
    pub cmd: String,
    /// Arguments to pass to the command
    pub args: Vec<String>,
    /// Original executable path (for logging)
    pub original: PathBuf,
}

/// Detect the appropriate interpreter for script files
pub fn resolve_command(executable: &Path, auto_interpreter: bool) -> ResolvedCommand {
    if !auto_interpreter {
        return ResolvedCommand {
            cmd: executable.to_string_lossy().to_string(),
            args: vec![],
            original: executable.to_path_buf(),
        };
    }

    let extension = executable.extension().and_then(|e| e.to_str()).unwrap_or("");

    match extension {
        "py" => {
            let python_cmd = if cfg!(windows) { "python" } else { "python3" };
            ResolvedCommand {
                cmd: python_cmd.to_string(),
                args: vec![executable.to_string_lossy().to_string()],
                original: executable.to_path_buf(),
            }
        }
        "js" => ResolvedCommand {
            cmd: "node".to_string(),
            args: vec![executable.to_string_lossy().to_string()],
            original: executable.to_path_buf(),
        },
        _ => ResolvedCommand {
            cmd: executable.to_string_lossy().to_string(),
            args: vec![],
            original: executable.to_path_buf(),
        },
    }
}

/// Process handle with I/O streams
pub struct ProcessTransport {
    /// Child process handle
    child: Child,
    /// Stdin for sending data
    stdin: ChildStdin,
    /// Stdout for receiving data
    stdout: BufReader<ChildStdout>,
    /// Process configuration
    config: ProcessConfig,
    /// Resolved command info
    cmd_info: ResolvedCommand,
    /// Process ID
    pid: u32,
}

impl ProcessTransport {
    /// Spawn a new process with the given executable and configuration
    pub async fn spawn(executable: impl AsRef<Path>, config: ProcessConfig) -> Result<Self> {
        let executable = executable.as_ref();
        let cmd_info = resolve_command(executable, config.auto_interpreter);

        debug!(
            "Spawning process: {} {:?} (original: {:?})",
            cmd_info.cmd, cmd_info.args, cmd_info.original
        );

        let mut cmd = Command::new(&cmd_info.cmd);
        cmd.args(&cmd_info.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set environment variables
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        // Set working directory
        if let Some(ref cwd) = config.cwd {
            cmd.current_dir(cwd);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn: {:?}", cmd_info.original))?;

        let pid = child
            .id()
            .context("Failed to get process ID")?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to open stdin")?;

        let stdout = child
            .stdout
            .take()
            .context("Failed to open stdout")?;

        // Spawn stderr logging task if enabled
        if config.log_stderr {
            if let Some(stderr) = child.stderr.take() {
                tokio::spawn(Self::log_stderr(stderr, pid));
            }
        }

        debug!("Process spawned with PID {}", pid);

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            config,
            cmd_info,
            pid,
        })
    }

    /// Spawn with default configuration
    pub async fn spawn_default(executable: impl AsRef<Path>) -> Result<Self> {
        Self::spawn(executable, ProcessConfig::default()).await
    }

    /// Get the process ID
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Get the original executable path
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.cmd_info.original
    }

    /// Send a line to the process stdin
    pub async fn send_line(&mut self, line: &str) -> Result<()> {
        self.stdin
            .write_all(line.as_bytes())
            .await
            .context("Failed to write to stdin")?;
        self.stdin
            .write_all(b"\n")
            .await
            .context("Failed to write newline")?;
        self.stdin.flush().await.context("Failed to flush stdin")?;
        Ok(())
    }

    /// Read a line from the process stdout
    pub async fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let bytes_read = self
            .stdout
            .read_line(&mut line)
            .await
            .context("Failed to read from stdout")?;

        if bytes_read == 0 {
            return Err(anyhow::anyhow!("Process closed stdout (EOF)"));
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

    /// Check if the process is still running
    pub async fn is_running(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                debug!("Process[{}] exited with status: {:?}", self.pid, status);
                false
            }
            Err(e) => {
                warn!("Process[{}] error checking status: {}", self.pid, e);
                false
            }
        }
    }

    /// Gracefully shut down the process
    ///
    /// First attempts graceful shutdown by closing stdin, then waits
    /// for the process to exit. If it doesn't exit within the timeout,
    /// it force kills the process.
    pub async fn shutdown(mut self) -> Result<()> {
        // Close stdin to signal EOF to the process
        drop(self.stdin);

        // Wait for process to exit with timeout
        match timeout(self.config.kill_timeout, self.child.wait()).await {
            Ok(Ok(status)) => {
                debug!("Process[{}] exited gracefully: {:?}", self.pid, status);
                Ok(())
            }
            Ok(Err(e)) => {
                warn!("Process[{}] error waiting: {}", self.pid, e);
                Self::force_kill_child(&mut self.child, self.pid).await
            }
            Err(_) => {
                warn!(
                    "Process[{}] did not exit within {:?}, force killing",
                    self.pid, self.config.kill_timeout
                );
                Self::force_kill_child(&mut self.child, self.pid).await
            }
        }
    }

    /// Force kill the process immediately
    pub async fn kill(&mut self) -> Result<()> {
        Self::force_kill_child(&mut self.child, self.pid).await
    }

    /// Force kill a child process
    async fn force_kill_child(child: &mut Child, pid: u32) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::process::ChildExt;
            // Send SIGTERM first for graceful termination
            if let Some(id) = child.id() {
                unsafe {
                    libc::kill(id as i32, libc::SIGTERM);
                }
            }
            // Brief pause for graceful shutdown
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        match child.kill().await {
            Ok(()) => {
                debug!("Process[{}] force killed", pid);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
                // Process already exited
                debug!("Process[{}] already exited", pid);
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("Failed to kill process[{}]: {}", pid, e)),
        }
    }

    /// Log stderr output
    async fn log_stderr(stderr: tokio::process::ChildStderr, pid: u32) {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            trace!("Process[{}] stderr: {}", pid, line);
        }
    }
}

/// Builder for process transport with fluent API
pub struct ProcessTransportBuilder {
    config: ProcessConfig,
}

impl ProcessTransportBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            config: ProcessConfig::default(),
        }
    }

    /// Set an environment variable
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.env.insert(key.into(), value.into());
        self
    }

    /// Set working directory
    pub fn cwd(mut self, path: impl AsRef<Path>) -> Self {
        self.config.cwd = Some(path.as_ref().to_path_buf());
        self
    }

    /// Enable/disable stderr logging
    pub fn log_stderr(mut self, enabled: bool) -> Self {
        self.config.log_stderr = enabled;
        self
    }

    /// Set kill timeout
    pub fn kill_timeout(mut self, secs: u64) -> Self {
        self.config.kill_timeout = Duration::from_secs(secs);
        self
    }

    /// Enable/disable auto interpreter detection
    pub fn auto_interpreter(mut self, enabled: bool) -> Self {
        self.config.auto_interpreter = enabled;
        self
    }

    /// Spawn the process
    pub async fn spawn(self, executable: impl AsRef<Path>) -> Result<ProcessTransport> {
        ProcessTransport::spawn(executable, self.config).await
    }
}

impl Default for ProcessTransportBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_command_binary() {
        let path = PathBuf::from("/usr/bin/myapp");
        let resolved = resolve_command(&path, true);
        assert_eq!(resolved.cmd, "/usr/bin/myapp");
        assert!(resolved.args.is_empty());
    }

    #[test]
    fn test_resolve_command_python() {
        let path = PathBuf::from("/tools/my_tool.py");
        let resolved = resolve_command(&path, true);
        let expected = if cfg!(windows) { "python" } else { "python3" };
        assert_eq!(resolved.cmd, expected);
        assert_eq!(resolved.args, vec!["/tools/my_tool.py"]);
    }

    #[test]
    fn test_resolve_command_node() {
        let path = PathBuf::from("/tools/my_tool.js");
        let resolved = resolve_command(&path, true);
        assert_eq!(resolved.cmd, "node");
        assert_eq!(resolved.args, vec!["/tools/my_tool.js"]);
    }

    #[test]
    fn test_resolve_command_no_auto() {
        let path = PathBuf::from("/tools/my_tool.py");
        let resolved = resolve_command(&path, false);
        assert_eq!(resolved.cmd, "/tools/my_tool.py");
        assert!(resolved.args.is_empty());
    }

    #[test]
    fn test_process_config_builder() {
        let config = ProcessConfig::new()
            .env("KEY", "value")
            .cwd("/tmp")
            .log_stderr(false)
            .kill_timeout(10);

        assert_eq!(config.env.get("KEY"), Some(&"value".to_string()));
        assert_eq!(config.cwd, Some(PathBuf::from("/tmp")));
        assert!(!config.log_stderr);
        assert_eq!(config.kill_timeout, Duration::from_secs(10));
    }
}
