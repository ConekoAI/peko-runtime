//! Command-executing hook handler for general extensions.
//!
//! This handler lets an extension manifest declare a `command` for a hook
//! point. When the hook fires, the command is spawned inside the extension
//! root, its stdout is captured, and the result is turned into a
//! [`HookOutput`].
//!
//! The primary use case is the `session.start` hook used by Superpowers:
//! `hooks/session-start` prints JSON containing `additionalContext`, which is
//! then injected into the system prompt via `{{session_context}}`.

use crate::extensions::framework::core::{
    context::HookContext, handler::HookHandler, hook_points::HookPoint,
};
use crate::extensions::framework::types::{HookInput, HookOutput, HookResult};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Default timeout for hook commands, in seconds.
pub const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 30;

/// Maximum stdout bytes to capture from a hook command.
pub const MAX_COMMAND_OUTPUT_BYTES: usize = 256 * 1024;

/// Expected output format for a command hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandOutputFormat {
    /// Parse stdout as JSON and extract `additionalContext`.
    #[default]
    Json,
    /// Treat stdout as plain text.
    Text,
}

impl CommandOutputFormat {
    /// Resolve a format string. Unrecognized values fall back to JSON.
    #[must_use]
    pub fn from_str_opt(s: Option<&str>, point: &HookPoint) -> Self {
        match s {
            Some("text") => Self::Text,
            Some("json") => Self::Json,
            Some(other) => {
                warn!("Unknown command output format '{}', using JSON", other);
                Self::Json
            }
            None => {
                // Default to JSON for session.start; text everywhere else.
                if matches!(point, HookPoint::SessionStart) {
                    Self::Json
                } else {
                    Self::Text
                }
            }
        }
    }
}

/// Configuration for a command hook.
#[derive(Debug, Clone)]
pub struct CommandHookConfig {
    /// Program or script to execute. Relative paths are resolved against the
    /// extension root.
    pub command: String,

    /// Arguments passed to the command.
    pub args: Vec<String>,

    /// Extra environment variables merged on top of the process environment.
    pub env: HashMap<String, String>,

    /// Timeout in seconds.
    pub timeout_secs: u64,

    /// How to interpret stdout.
    pub output_format: CommandOutputFormat,
}

/// Handler that runs an external command to produce hook output.
#[derive(Debug, Clone)]
pub struct CommandHookHandler {
    config: CommandHookConfig,
    extension_dir: PathBuf,
    hook_point: HookPoint,
}

impl CommandHookHandler {
    /// Create a new command hook handler.
    #[must_use]
    pub fn new(
        config: CommandHookConfig,
        extension_dir: impl Into<PathBuf>,
        hook_point: HookPoint,
    ) -> Self {
        Self {
            config,
            extension_dir: extension_dir.into(),
            hook_point,
        }
    }

    /// Resolve the command path. Absolute paths are kept as-is; relative paths
    /// are joined to the extension root.
    fn resolve_command_path(&self) -> PathBuf {
        let raw = &self.config.command;
        let candidate = Path::new(raw);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.extension_dir.join(raw)
        }
    }

    /// Build the environment for the child process.
    fn build_env(&self,
        ctx: &HookContext,
    ) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();

        // Standard Peko hook variables.
        env.insert("PEKO_HOOK_POINT".to_string(), self.hook_point.name());
        env.insert(
            "PEKO_EXTENSION_DIR".to_string(),
            self.extension_dir.to_string_lossy().to_string(),
        );

        // Event and workspace from the hook input.
        let (event, workspace) = self.extract_event_and_workspace(ctx);
        if let Some(event) = event {
            env.insert("PEKO_HOOK_EVENT".to_string(), event);
        }
        if let Some(workspace) = workspace {
            env.insert("PEKO_WORKSPACE".to_string(), workspace);
        }

        // Principal id from tool context state if available.
        if let Some(tc) = ctx
            .get_state::<crate::extensions::framework::types::ToolRuntimeContext>("tool_context")
        {
            if let Some(pid) = &tc.principal_id {
                env.insert("PEKO_PRINCIPAL_ID".to_string(), pid.clone());
            }
        }

        // Superpowers compatibility: the upstream script branches on this.
        env.insert(
            "CLAUDE_PLUGIN_ROOT".to_string(),
            self.extension_dir.to_string_lossy().to_string(),
        );

        // Manifest-supplied env overrides everything above.
        for (k, v) in &self.config.env {
            env.insert(k.clone(), v.clone());
        }

        env
    }

    /// Pull event/workspace out of the hook input when possible.
    fn extract_event_and_workspace(
        &self,
        ctx: &HookContext,
    ) -> (Option<String>, Option<String>) {
        match &ctx.input {
            HookInput::SessionState(snapshot) => {
                let event = snapshot
                    .metadata
                    .get("event")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let workspace = snapshot
                    .metadata
                    .get("workspace")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                (event, workspace)
            }
            HookInput::PromptBuild(state) => (
                Some("prompt_build".to_string()),
                Some(state.workspace.to_string_lossy().to_string()),
            ),
            _ => (None, None),
        }
    }

    /// Run the configured command and return its stdout as a string, or
    /// `None` if the command failed or timed out.
    async fn run_command(&self,
        ctx: &HookContext,
    ) -> Option<String> {
        let program = self.resolve_command_path();
        let env = self.build_env(ctx);

        info!(
            extension = %self.extension_dir.display(),
            command = %program.display(),
            "Executing hook command"
        );

        let mut cmd = Command::new(&program);
        cmd.args(&self.config.args)
            .current_dir(&self.extension_dir)
            .env_clear()
            .envs(&env);

        let timeout = Duration::from_secs(self.config.timeout_secs);

        let output = tokio::time::timeout(timeout, cmd.output()).await;

        match output {
            Ok(Ok(output)) => {
                if !output.stderr.is_empty() {
                    let stderr = String::from_utf8_lossy(
                        &output.stderr[..output.stderr.len().min(2048)]
                    );
                    debug!(stderr = %stderr, "Hook command stderr");
                }

                if !output.status.success() {
                    warn!(
                        command = %program.display(),
                        exit = ?output.status.code(),
                        "Hook command exited with non-zero status"
                    );
                    return None;
                }

                let capped = &output.stdout[..output.stdout.len().min(MAX_COMMAND_OUTPUT_BYTES)];
                Some(String::from_utf8_lossy(capped).into_owned())
            }
            Ok(Err(e)) => {
                warn!("Hook command '{}' failed to run: {}", program.display(), e);
                None
            }
            Err(_) => {
                warn!(
                    command = %program.display(),
                    timeout_secs = self.config.timeout_secs,
                    "Hook command timed out"
                );
                None
            }
        }
    }

    fn parse_output(
        &self,
        stdout: &str,
    ) -> Option<String> {
        match self.config.output_format {
            CommandOutputFormat::Text => Some(stdout.to_string()),
            CommandOutputFormat::Json => {
                Self::extract_additional_context(stdout).or_else(|| {
                    // If JSON extraction fails, fall back to raw stdout so a
                    // plain-text hook still produces useful output.
                    Some(stdout.to_string())
                })
            }
        }
    }

    /// Parse JSON looking for the Superpowers-compatible shapes:
    /// - `{"additionalContext": "..."}`
    /// - `{"hookSpecificOutput": {"hookEventName": "...", "additionalContext": "..."}}`
    fn extract_additional_context(stdout: &str) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
        value
            .get("additionalContext")
            .or_else(|| value.get("hookSpecificOutput")?.get("additionalContext"))
            .and_then(|v| v.as_str())
            .map(String::from)
    }
}

#[async_trait]
impl HookHandler for CommandHookHandler {
    async fn handle(&self,
        ctx: HookContext,
    ) -> HookResult {
        match self.run_command(&ctx).await {
            Some(stdout) => match self.parse_output(&stdout) {
                Some(text) if !text.is_empty() => {
                    debug!(
                        hook = %self.hook_point.name(),
                        bytes = text.len(),
                        "Command hook produced output"
                    );
                    HookResult::Continue(HookOutput::Text(text))
                }
                _ => {
                    debug!("Command hook produced empty output; passing through");
                    HookResult::PassThrough
                }
            },
            None => {
                debug!("Command hook failed; passing through");
                HookResult::PassThrough
            }
        }
    }

    fn hook_point(&self) -> HookPoint {
        self.hook_point.clone()
    }

    fn priority(&self) -> i32 {
        100
    }

    fn name(&self) -> String {
        format!(
            "{}:command:{}",
            self.extension_dir.display(),
            self.config.command
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_additional_context_plain_json() {
        let text = CommandHookHandler::extract_additional_context(
            r#"{"additionalContext": "hello world"}"#,
        );
        assert_eq!(text, Some("hello world".to_string()));
    }

    #[test]
    fn extract_additional_context_claude_code_shape() {
        let text = CommandHookHandler::extract_additional_context(
            r#"{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "bootstrap"}}"#,
        );
        assert_eq!(text, Some("bootstrap".to_string()));
    }

    #[test]
    fn extract_additional_context_missing_returns_none() {
        let text = CommandHookHandler::extract_additional_context(
            r#"{"foo": "bar"}"#,
        );
        assert_eq!(text, None);
    }

    #[test]
    fn output_format_defaults() {
        assert_eq!(
            CommandOutputFormat::from_str_opt(None, &HookPoint::SessionStart),
            CommandOutputFormat::Json
        );
        assert_eq!(
            CommandOutputFormat::from_str_opt(None, &HookPoint::AgentInit),
            CommandOutputFormat::Text
        );
        assert_eq!(
            CommandOutputFormat::from_str_opt(Some("text"), &HookPoint::SessionStart),
            CommandOutputFormat::Text
        );
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use crate::extensions::framework::core::ExtensionServices;
        use crate::extensions::framework::types::SessionSnapshot;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use tempfile::TempDir;

        fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
            let path = dir.join(name);
            fs::write(&path, body).unwrap();
            let mut perms = fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
            path
        }

        fn session_ctx(event: &str, workspace: &str) -> HookContext {
            let mut metadata = std::collections::HashMap::new();
            metadata.insert("event".to_string(), serde_json::json!(event));
            metadata.insert("workspace".to_string(), serde_json::json!(workspace));
            let snapshot = SessionSnapshot {
                session_id: "test-session".to_string(),
                message_count: 0,
                context_tokens: 0,
                metadata,
            };
            HookContext::new(
                HookPoint::SessionStart,
                HookInput::SessionState(snapshot),
                Arc::new(ExtensionServices::new()),
            )
        }

        #[tokio::test]
        async fn command_hook_returns_additional_context() {
            let tmp = TempDir::new().unwrap();
            write_script(
                tmp.path(),
                "hook.sh",
                r#"#!/bin/sh
echo '{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "bootstrap"}}'
"#,
            );

            let handler = CommandHookHandler::new(
                CommandHookConfig {
                    command: "./hook.sh".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    timeout_secs: 5,
                    output_format: CommandOutputFormat::Json,
                },
                tmp.path(),
                HookPoint::SessionStart,
            );

            let result = handler.handle(session_ctx("startup", "/tmp/ws")).await;
            match result {
                HookResult::Continue(HookOutput::Text(text)) => {
                    assert_eq!(text, "bootstrap");
                }
                other => panic!("expected Continue(Text), got {:?}", other),
            }
        }

        #[tokio::test]
        async fn command_hook_passes_through_on_failure() {
            let tmp = TempDir::new().unwrap();
            write_script(tmp.path(), "fail.sh", "#!/bin/sh\nexit 1\n");

            let handler = CommandHookHandler::new(
                CommandHookConfig {
                    command: "./fail.sh".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    timeout_secs: 5,
                    output_format: CommandOutputFormat::Json,
                },
                tmp.path(),
                HookPoint::SessionStart,
            );

            let result = handler.handle(session_ctx("startup", "/tmp/ws")).await;
            assert!(
                matches!(result, HookResult::PassThrough),
                "expected PassThrough on failure, got {:?}",
                result
            );
        }

        #[tokio::test]
        async fn command_hook_sets_superpowers_env_var() {
            let tmp = TempDir::new().unwrap();
            write_script(
                tmp.path(),
                "env.sh",
                "#!/bin/sh\necho \"$CLAUDE_PLUGIN_ROOT\"\n",
            );

            let handler = CommandHookHandler::new(
                CommandHookConfig {
                    command: "./env.sh".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    timeout_secs: 5,
                    output_format: CommandOutputFormat::Text,
                },
                tmp.path(),
                HookPoint::SessionStart,
            );

            let result = handler.handle(session_ctx("startup", "/tmp/ws")).await;
            let expected = tmp.path().to_string_lossy().to_string();
            match result {
                HookResult::Continue(HookOutput::Text(text)) => {
                    assert_eq!(text.trim(), expected);
                }
                other => panic!("expected Continue(Text), got {:?}", other),
            }
        }

        #[tokio::test]
        async fn command_hook_respects_timeout() {
            let tmp = TempDir::new().unwrap();
            write_script(tmp.path(), "slow.sh", "#!/bin/sh\nsleep 60\n");

            let handler = CommandHookHandler::new(
                CommandHookConfig {
                    command: "./slow.sh".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    timeout_secs: 1,
                    output_format: CommandOutputFormat::Text,
                },
                tmp.path(),
                HookPoint::SessionStart,
            );

            let start = std::time::Instant::now();
            let result = handler.handle(session_ctx("startup", "/tmp/ws")).await;
            let elapsed = start.elapsed();
            assert!(matches!(result, HookResult::PassThrough));
            assert!(elapsed.as_secs() < 5, "timeout should fire quickly, took {:?}", elapsed);
        }
    }
}
