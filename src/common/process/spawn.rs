//! Process spawning with interpreter auto-detection

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, trace};

use super::config::ProcessSpawnConfig;

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
#[must_use]
pub fn resolve_command(executable: &Path, auto_interpreter: bool) -> ResolvedCommand {
    if !auto_interpreter {
        return ResolvedCommand {
            cmd: executable.to_string_lossy().to_string(),
            args: vec![],
            original: executable.to_path_buf(),
        };
    }

    let extension = executable
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

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

/// Spawn a child process with the given configuration
///
/// Returns the child handle, stdin, stdout, and resolved command info.
/// Stderr is optionally logged via a background task.
pub async fn spawn_process(
    config: &ProcessSpawnConfig,
) -> Result<(Child, ChildStdin, BufReader<ChildStdout>, u32)> {
    let executable = Path::new(&config.command);
    let cmd_info = resolve_command(executable, config.auto_interpreter);

    debug!(
        "Spawning process: {} {:?} (original: {:?})",
        cmd_info.cmd, cmd_info.args, cmd_info.original
    );

    let mut cmd = Command::new(&cmd_info.cmd);
    // Use resolved args for interpreter auto-detection, but fall back to config args
    // when the command is already the interpreter (e.g., "node" with args ["gateway.js"])
    let args: Vec<String> = if cmd_info.args.is_empty() {
        config.args.clone()
    } else {
        cmd_info.args.clone()
    };
    cmd.args(&args)
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

    let pid = child.id().context("Failed to get process ID")?;

    let stdin = child.stdin.take().context("Failed to open stdin")?;
    let stdout = child.stdout.take().context("Failed to open stdout")?;

    // Spawn stderr logging task if enabled
    if config.log_stderr {
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(log_stderr(stderr, pid));
        }
    }

    debug!("Process spawned with PID {}", pid);

    Ok((child, stdin, BufReader::new(stdout), pid))
}

/// Log stderr output
async fn log_stderr(stderr: tokio::process::ChildStderr, pid: u32) {
    let reader = BufReader::new(stderr);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        trace!("Process[{}] stderr: {}", pid, line);
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
}
