//! `Bash` tool - Execute system shell commands
//!
//! Implements ADR-014: All-or-nothing permission model
//! - Full shell access via system shell (sh/bash on Unix, cmd on Windows)
//! - No sandboxing, no command blocking, no env filtering
//! - Security boundary is tool enablement (enabled = full access)
//!
//! Supports both blocking execution and `run_in_background` for parity with
//! Claude Code's `Bash` tool. `run_in_background` spawns the command through
//! the caller principal's background spawner — the same path as
//! `Async action=spawn tool=Bash` — and the task body then streams its output
//! into the task's live buffer. Poll with the Async family (output, status,
//! stop, list); blocking calls are bounded only by `timeout`.

use crate::tools::builtin::process::spawn_in_own_group;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::process::Command;

use peko_tools_core::{BackgroundSpawn, Tool, ToolContext};

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

/// Default cap for stdout/stderr returned in a single blocking call.
/// Per-call override via `BashArgs::max_output_bytes`.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 100_000;

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
    /// Optional cap (in bytes) for stdout and stderr returned to the
    /// caller. Applies independently to each stream. When the limit is
    /// hit, the truncated stream is suffixed with `...(truncated)` and
    /// `stdout_truncated` / `stderr_truncated` are set to `true` in the
    /// response. Defaults to [`DEFAULT_MAX_OUTPUT_BYTES`]. Ignored for
    /// `run_in_background: true` (use `Async action output` with `tail_lines`
    /// to read slices of large outputs).
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
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

    /// Execute a shell command with an optional per-call timeout.
    ///
    /// `ctx` is observed for soft-interrupt: when the engine has plumbed
    /// a `CancellationToken` (PR #128) into the tool layer via
    /// `ToolDispatcher`'s `for_hook_run_with_abort` path, a cancel
    /// during a long-running subprocess aborts the wait, drops the
    /// `Command` future (which Tokio then uses to kill the child), and
    /// returns `Err`. Without a context, the call is uninterruptible —
    /// preserving the legacy CLI path that has no cancel token.
    async fn execute_command_blocking(
        command: &str,
        working_dir: Option<std::path::PathBuf>,
        timeout_ms: Option<u64>,
        max_output_bytes: Option<usize>,
        ctx: Option<&ToolContext>,
    ) -> Result<serde_json::Value> {
        let mut cmd = Command::new(SHELL);
        cmd.arg(SHELL_ARG).arg(command);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Build the abort-watcher future. When no context is supplied
        // (or the context carries no abort receiver) the future is
        // `pending` and the select behaves as a 2-way race.
        let mut abort_rx = ctx.map(ToolContext::abort_signal);

        // Timeout, abort, and a dropped call (executor shutdown) all return
        // early and drop `tree`, killing everything the command started.
        let (child, tree) =
            spawn_in_own_group(&mut cmd).context("Failed to execute Bash command")?;
        let output_fut = child.wait_with_output();
        tokio::pin!(output_fut);

        let output = match timeout_ms {
            Some(ms) if ms > 0 => {
                let timeout_sleep = tokio::time::sleep(tokio::time::Duration::from_millis(ms));
                tokio::pin!(timeout_sleep);
                tokio::select! {
                    res = &mut output_fut => res.context("Failed to execute Bash command")?,
                    () = &mut timeout_sleep => {
                        return Err(anyhow::anyhow!("Bash command timed out after {ms} ms"));
                    }
                    () = wait_for_abort(&mut abort_rx) => {
                        return Err(anyhow::anyhow!("Bash command aborted"));
                    }
                }
            }
            _ => tokio::select! {
                res = &mut output_fut => res.context("Failed to execute Bash command")?,
                () = wait_for_abort(&mut abort_rx) => {
                    return Err(anyhow::anyhow!("Bash command aborted"));
                }
            },
        };
        tree.disarm();

        Self::format_output(&output, max_output_bytes)
    }

    /// Start the command as a background task owned by the caller's
    /// principal: the call re-enters this tool as the task body (see
    /// [`ToolContext::background`]), exactly like `Async action=spawn`.
    async fn spawn_background(
        params: serde_json::Value,
        timeout_ms: Option<u64>,
        ctx: Option<&ToolContext>,
    ) -> Result<serde_json::Value> {
        let (Some(ctx), Some(spawner)) = (ctx, ctx.and_then(|c| c.background.spawner.clone()))
        else {
            anyhow::bail!(
                "Bash run_in_background needs the calling principal's Async runtime; \
                 none is available for this call"
            );
        };
        let mut params = params;
        if let Some(object) = params.as_object_mut() {
            object.remove("run_in_background");
            object.remove("timeout");
        }
        let task_id = spawner
            .spawn(
                BackgroundSpawn {
                    tool: "Bash".to_string(),
                    params,
                    timeout_millis: timeout_ms,
                },
                ctx,
            )
            .await?;
        Ok(json!({
            "task_id": task_id,
            "status": "running",
            "tool": "Bash",
        }))
    }

    /// Background execution with live progress capture (§4.1).
    ///
    /// Unlike [`Self::execute_command_blocking`] — which buffers the whole
    /// child output and only returns it at exit — this variant reads the
    /// child's stdout/stderr incrementally and mirrors what it reads into
    /// the task's shared progress buffer. On cancel/timeout the executor
    /// therefore has something to deliver besides an opaque error, and
    /// `Async action output` on a still-running task shows live output.
    ///
    /// The returned JSON has the same shape as
    /// [`Self::format_output`] so callers cannot tell the paths apart.
    async fn execute_command_streaming(
        command: &str,
        working_dir: Option<std::path::PathBuf>,
        abort_rx: tokio::sync::watch::Receiver<bool>,
        progress: Arc<std::sync::Mutex<String>>,
    ) -> Result<serde_json::Value> {
        use tokio::io::AsyncReadExt;

        let mut cmd = Command::new(SHELL);
        cmd.arg(SHELL_ARG).arg(command);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Stop (abort), the task timeout, and executor shutdown drop `tree`,
        // killing everything the command started.
        let (mut child, tree) =
            spawn_in_own_group(&mut cmd).context("Failed to execute Bash command")?;
        let mut stdout_pipe = child.stdout.take().context("stdout not captured")?;
        let mut stderr_pipe = child.stderr.take().context("stderr not captured")?;

        // One reader task per stream, each mirroring into the shared
        // progress buffer. `from_utf8_lossy` per chunk can emit a
        // replacement char at a chunk boundary that splits a multi-byte
        // character — acceptable for a progress display, and the final
        // result below is built from the exact bytes.
        let progress_for_stdout = Arc::clone(&progress);
        let stdout_task = tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match stdout_pipe.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        peko_tools_core::background::append_progress(
                            &progress_for_stdout,
                            &String::from_utf8_lossy(&chunk[..n]),
                        );
                    }
                }
            }
            buf
        });
        let progress_for_stderr = Arc::clone(&progress);
        let stderr_task = tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match stderr_pipe.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        peko_tools_core::background::append_progress(
                            &progress_for_stderr,
                            &String::from_utf8_lossy(&chunk[..n]),
                        );
                    }
                }
            }
            buf
        });

        // Race the child against the executor's cancel channel. On abort
        // the child is killed explicitly (the reader tasks then see EOF
        // and drain), and we bail exactly like the blocking path.
        let mut abort_rx = abort_rx;
        let (status, stdout_bytes, stderr_bytes) = tokio::select! {
            res = child.wait() => {
                let status = res.context("Failed to execute Bash command")?;
                tree.disarm();
                let stdout_bytes = stdout_task
                    .await
                    .unwrap_or_default();
                let stderr_bytes = stderr_task
                    .await
                    .unwrap_or_default();
                (status, stdout_bytes, stderr_bytes)
            }
            () = async {
                let _ = abort_rx.changed().await;
            } => {
                drop(tree);
                let _ = child.wait().await;
                return Err(anyhow::anyhow!("Bash command aborted"));
            }
        };

        let output = std::process::Output {
            status,
            stdout: stdout_bytes,
            stderr: stderr_bytes,
        };
        Self::format_output(&output, None)
    }

    /// Format command output
    fn format_output(
        output: &std::process::Output,
        max_output_bytes: Option<usize>,
    ) -> Result<serde_json::Value> {
        let stdout_raw = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr_raw = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        let limit = max_output_bytes.unwrap_or(DEFAULT_MAX_OUTPUT_BYTES);

        let (stdout, stdout_truncated) = truncate_with_marker(&stdout_raw, limit);
        let (stderr, stderr_truncated) = truncate_with_marker(&stderr_raw, limit);

        Ok(json!({
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated,
            "success": output.status.success(),
        }))
    }

    /// Core execution dispatcher.
    async fn execute_with_maybe_context(
        &self,
        params: serde_json::Value,
        ctx: Option<&ToolContext>,
    ) -> Result<serde_json::Value> {
        let args: BashArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let cwd = self.resolve_cwd(args.cwd.as_deref());

        // A background-task body streams into the task's live buffer under
        // the task's timeout and cancellation; `run_in_background` inside a
        // task is moot (it is already background).
        if let Some((ctx, progress)) = ctx.and_then(|c| Some((c, c.background.progress.clone()?))) {
            Self::execute_command_streaming(&args.command, cwd, ctx.abort_signal(), progress).await
        } else if args.run_in_background {
            Self::spawn_background(params, args.timeout, ctx).await
        } else {
            Self::execute_command_blocking(
                &args.command,
                cwd,
                args.timeout,
                args.max_output_bytes,
                ctx,
            )
            .await
        }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve to `()` when the abort receiver signals; never resolves
/// when the receiver is `None` (no context supplied). Used as one
/// branch of the `tokio::select!` inside `execute_command_blocking`.
async fn wait_for_abort(rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    if let Some(rx) = rx.as_mut() {
        // `changed` resolves when the watcher's value flips. We don't
        // care about the new value — we just want to bail.
        let _ = rx.changed().await;
    } else {
        // No abort context — park forever. The other select branches
        // (timeout, command output) still race as normal.
        std::future::pending::<()>().await;
    }
}

/// Truncate a stream at `limit` bytes and append a `...(truncated)` marker.
/// Returns `(value, was_truncated)`. Walks back to a UTF-8 char boundary so
/// the returned string is always valid UTF-8.
fn truncate_with_marker(s: &str, limit: usize) -> (String, bool) {
    if s.len() <= limit {
        return (s.to_string(), false);
    }
    let mut cut = limit;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    (format!("{}...(truncated)", &s[..cut]), true)
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

## API
```json
{{
    "command": "your command here",
    "description": "what the command does",
    "cwd": "./subdir",
    "run_in_background": false,
    "timeout": 60000,
    "max_output_bytes": 100000
}}
```

## Output truncation

Stdout and stderr are each capped at `max_output_bytes` (default 100000).
When a stream is truncated, it ends with `...(truncated)` and the response
sets `stdout_truncated: true` and/or `stderr_truncated: true`. If you
expect large output, prefer `run_in_background: true` and read it with
`Async action output` + `tail_lines` instead of raising the cap.

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
To monitor or cancel the backgrounded command, use Async:

- `Async({{action: "status", task_id}})` — one-shot status (pending / running /
  completed / failed / cancelled / timed_out)
- `Async({{action: "output", task_id, block?, timeout?, tail_lines?}})` — read
  the result; with `block: true` the call waits until the task
  reaches a terminal state
- `Async({{action: "stop", task_id}})` — cancel a still-running task; returns
  `success: true, already_terminal: true` if the task is already done
- `Async({{action: "list", status_filter?, tool_filter?}})` — enumerate all
  background tasks owned by the current principal

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
                },
                "max_output_bytes": {
                    "type": "integer",
                    "description": "Optional cap (in bytes) for stdout and stderr returned in the response. Each stream is truncated independently and flagged via stdout_truncated / stderr_truncated. Defaults to 100000. Ignored when run_in_background is true.",
                    "minimum": 1
                }
            },
            "required": ["command"]
        })
    }

    /// F33: shell tool — opt out of parallel dispatch. Concurrent
    /// `Bash` calls share cwd, env, and child-process state; `cat x |
    /// tee x` style commands race on file handles; two `cd` commands
    /// step on each other. Serializing keeps each command's view of
    /// the world coherent.
    fn parallelizable(&self) -> bool {
        false
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
    async fn run_in_background_without_a_spawner_is_refused() {
        // Background tasks are always owned by a principal: without the
        // caller's Async runtime there is nowhere to register one.
        let error = BashTool::new()
            .execute(json!({"command": "echo nope", "run_in_background": true}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Async runtime"), "{error}");
    }

    /// A long-running chatty task keeps only the newest output in its
    /// progress buffer instead of growing it for its whole lifetime.
    #[cfg(unix)]
    #[tokio::test]
    async fn task_body_progress_is_capped_to_the_newest_output() {
        let progress: Arc<std::sync::Mutex<String>> = Arc::default();
        let mut ctx = ToolContext::default_for_tool("Bash");
        ctx.background.progress = Some(Arc::clone(&progress));
        BashTool::new()
            .execute_with_context(
                json!({"command": "head -c 300000 /dev/zero | tr '\\0' x; echo; echo LAST"}),
                &ctx,
            )
            .await
            .unwrap();
        let buf = progress.lock().unwrap();
        assert!(
            buf.len() <= peko_tools_core::background::PROGRESS_MAX_BYTES,
            "progress grew to {} bytes",
            buf.len()
        );
        assert!(
            buf.ends_with("LAST\n"),
            "newest output kept: {:?}",
            &buf[buf.len() - 10..]
        );
    }

    /// Stopping, or timing out, a command kills everything it started,
    /// not just the shell.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_and_timeout_kill_the_whole_process_tree() {
        use crate::tools::builtin::process::tests::{
            long_lived_child, still_running_after_grace, wait_for_pid_file,
        };
        let dir = tempfile::tempdir().unwrap();
        for (case, background, stop) in [
            ("foreground stop", false, true),
            ("foreground timeout", false, false),
            ("background task stop", true, true),
        ] {
            let pid_file = dir.path().join(format!("{}.pid", case.replace(' ', "_")));
            let (tx, rx) = tokio::sync::watch::channel(false);
            let mut ctx = ToolContext::default_for_tool("Bash").with_abort_signal(rx);
            if background {
                ctx.background.progress = Some(Arc::default());
            }
            let mut params = json!({ "command": long_lived_child(&pid_file) });
            if !stop {
                params["timeout"] = json!(500);
            }
            let call =
                tokio::spawn(
                    async move { BashTool::new().execute_with_context(params, &ctx).await },
                );
            wait_for_pid_file(&pid_file).await;
            if stop {
                tx.send(true).unwrap();
            }
            let result = call.await.unwrap();
            assert!(result.is_err(), "{case}: {result:?}");
            assert!(
                !still_running_after_grace(&pid_file).await,
                "{case}: the command's child outlived the call"
            );
        }
    }

    /// A command that exits on its own leaves processes it deliberately
    /// backgrounded running.
    #[cfg(unix)]
    #[tokio::test]
    async fn normal_exit_leaves_deliberate_background_processes() {
        use crate::tools::builtin::process::tests::still_running_after_grace;
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("server.pid");
        let command = format!(
            "sleep 30 > /dev/null 2>&1 & echo $! > '{}'",
            pid_file.display()
        );
        let result = BashTool::new()
            .execute_with_context(
                json!({ "command": command }),
                &ToolContext::default_for_tool("Bash"),
            )
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0, "{result}");
        assert!(
            still_running_after_grace(&pid_file).await,
            "a backgrounded process must survive its command's normal exit"
        );
    }

    #[tokio::test]
    async fn task_body_streams_into_the_progress_buffer() {
        let progress: Arc<std::sync::Mutex<String>> = Arc::default();
        let mut ctx = ToolContext::default_for_tool("Bash");
        ctx.background.progress = Some(Arc::clone(&progress));
        // `max_output_bytes` does not apply to a background task body.
        let result = BashTool::new()
            .execute_with_context(
                json!({"command": "echo streamed", "max_output_bytes": 1}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            result["stdout"].as_str().unwrap().contains("streamed"),
            "{result}"
        );
        assert!(progress.lock().unwrap().contains("streamed"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_bash_max_output_bytes_truncates_and_flags() {
        let tool = BashTool::new();

        // Produce 200 bytes of stdout with a small per-call cap.
        // Use POSIX-portable utilities so this works under dash
        // (Ubuntu's /bin/sh), not just bash — brace expansion like
        // `{1..200}` is bash-only and silently produces 1 byte on dash.
        let params = json!({
            "command": "head -c 200 < /dev/zero | tr '\\0' x",
            "max_output_bytes": 32,
        });

        let result = tool.execute(params).await.unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.ends_with("...(truncated)"), "stdout: {stdout}");
        assert_eq!(result["stdout_truncated"], true);
        assert_eq!(result["stderr_truncated"], false);
    }

    #[test]
    fn truncate_with_marker_under_limit_is_unchanged() {
        let (out, truncated) = truncate_with_marker("hi", 100);
        assert_eq!(out, "hi");
        assert!(!truncated);
    }

    #[test]
    fn truncate_with_marker_respects_utf8_boundary() {
        // "é" is 2 bytes in UTF-8 (0xC3 0xA9). A limit that lands in the
        // middle of it should walk back to the char boundary.
        let s = "éééé"; // 8 bytes
        let (out, truncated) = truncate_with_marker(s, 3);
        assert!(truncated);
        // 3 lands inside the first 2-byte char; we should cut at 0 or 2.
        assert!(out.starts_with("é") || out.starts_with(""));
        assert!(out.ends_with("...(truncated)"));
    }

    /// Pre-armed abort signal — subscribe first, then abort, then
    /// run `sleep 60` via `BashTool::execute_with_maybe_context`,
    /// assert the call returns `Err` within ~1s — not the 60s the
    /// command would otherwise block for. Validates the
    /// `tokio::select!` between the command and the abort watcher
    /// in `execute_command_blocking`.
    ///
    /// Note: we subscribe *before* calling `abort()` so the
    /// receiver's initial state is `false` and `rx.changed().await`
    /// (the path used by `wait_for_abort`) resolves on the abort
    /// edge. Subscribing after `abort()` would leave the receiver
    /// initialized to `true` and `changed()` would never fire.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_aborts_long_command() {
        use std::time::{Duration, Instant};

        let tool = BashTool::new();
        let abort = peko_tools_core::AbortSignal::new();
        let ctx = peko_tools_core::ToolContext::for_hook_run_with_abort(
            "abort_test",
            "Bash",
            "Bash",
            abort.subscribe(),
        );
        // Flip the signal after subscribing.
        abort.abort();

        let start = Instant::now();
        let result = tool
            .execute_with_maybe_context(json!({"command": "sleep 60"}), Some(&ctx))
            .await;
        let elapsed = start.elapsed();

        // Must abort quickly — well under the 60s the command would
        // otherwise take. Generous bound to avoid CI flakes.
        assert!(
            elapsed < Duration::from_secs(2),
            "aborted call took {elapsed:?}; expected < 2s"
        );
        let err = result.expect_err("aborted command should error");
        let msg = err.to_string();
        assert!(msg.contains("abort"), "expected abort error, got: {msg}");
    }

    /// Without a context (or with a never-aborted one), the tool
    /// behaves as before — `sleep` runs to completion. This is the
    /// legacy non-cancelable path; we don't want the abort check to
    /// break it.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_no_ctx_runs_to_completion() {
        let tool = BashTool::new();
        let result = tool
            .execute_with_maybe_context(json!({"command": "echo hi"}), None)
            .await;
        assert!(result.is_ok());
        let v = result.unwrap();
        assert_eq!(v["success"], true);
    }
}
