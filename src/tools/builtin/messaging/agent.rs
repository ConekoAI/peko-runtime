//! Agent tool (Claude Code parity)
//!
//! Spawns subagent sessions for isolated task execution.
//! Results are announced back to the parent via the event system.
//!
//! Note: Async execution and timeout are handled by the framework-level
//! `AsyncExecutionRouter` using a constant 5-minute timeout. On timeout,
//! the work is detached to a background task automatically.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agents::agent_config::AgentConfig;
use crate::agents::subagent_error::SpawnError;
use crate::agents::subagent_executor::{ExecutionConfig, SubagentExecutor};
use crate::common::identifiers::parse_agent_name;
use crate::common::paths::PathResolver;
use crate::session::types::SpawnCleanupPolicy;
use crate::tools::core::Tool;
use crate::tools::{bridge_to_cancellation_token, CancellationTokenBridgeGuard, ToolContext};
use anyhow::Context;

/// Maximum allowed spawn depth (safety limit)
const DEFAULT_MAX_SPAWN_DEPTH: u32 = 3;

/// Maximum concurrent subagent runs per agent
const DEFAULT_MAX_CONCURRENT: usize = 5;

/// Trait for providing the current session key
///
/// This allows the tool to get the current session key at execution time,
/// even though the session is determined at runtime.
pub trait SessionKeyProvider: Send + Sync {
    /// Get the current session key
    fn current_session_key(&self) -> String;
}

/// Simple session key provider that returns a static key
pub struct StaticSessionKeyProvider {
    session_key: String,
}

impl StaticSessionKeyProvider {
    #[must_use]
    pub fn new(session_key: impl Into<String>) -> Self {
        Self {
            session_key: session_key.into(),
        }
    }
}

impl SessionKeyProvider for StaticSessionKeyProvider {
    fn current_session_key(&self) -> String {
        self.session_key.clone()
    }
}

impl SessionKeyProvider for Arc<DynamicSessionKeyProvider> {
    fn current_session_key(&self) -> String {
        self.get_session_key()
    }
}

/// Dynamic session key provider that can be updated at runtime
///
/// This is useful when the session key is determined at runtime,
/// such as when processing messages from different sessions.
#[derive(Clone)]
pub struct DynamicSessionKeyProvider {
    session_key: Arc<std::sync::RwLock<String>>,
}

impl DynamicSessionKeyProvider {
    #[must_use]
    pub fn new(initial_key: impl Into<String>) -> Self {
        Self {
            session_key: Arc::new(std::sync::RwLock::new(initial_key.into())),
        }
    }

    /// Update the current session key
    pub fn set_session_key(&self, key: impl Into<String>) {
        if let Ok(mut guard) = self.session_key.write() {
            *guard = key.into();
        }
    }

    /// Get the current session key
    #[must_use]
    pub fn get_session_key(&self) -> String {
        self.session_key
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

impl SessionKeyProvider for DynamicSessionKeyProvider {
    fn current_session_key(&self) -> String {
        self.get_session_key()
    }
}

/// Agent tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentArgs {
    /// Task description / prompt for the subagent
    pub prompt: String,
    /// Subagent type: name of the agent config under ~/.peko/agents/<subagent_type>/config.toml
    pub subagent_type: String,
    /// Optional description for tracking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional model override for the subagent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Create isolated session without parent context
    #[serde(default)]
    pub isolated: bool,
    /// Cleanup policy: "keep" or "delete"
    #[serde(default)]
    pub cleanup: Option<String>,
    /// Parent session key (auto-detected if not provided)
    pub parent_session_key: Option<String>,
}

/// Agent tool
///
/// Creates a subagent session and executes a task in the background.
/// Results are announced back to the parent when complete.
pub struct AgentTool {
    /// Subagent executor for background execution
    executor: Arc<SubagentExecutor>,
    /// Optional principal workspace. When set, `subagent_type` resolution
    /// prefers principal-scoped `AGENT.md` files at
    /// `<workspace>/agents/<name>/...` before falling back to the global
    /// `~/.peko/agents/<name>/config.toml` layout.
    workspace: Option<PathBuf>,
    /// Session key provider to get current session at execution time
    session_provider: Option<Box<dyn SessionKeyProvider>>,
    /// Maximum spawn depth allowed
    max_depth: u32,
    /// Maximum concurrent runs
    max_concurrent: usize,
}

impl AgentTool {
    /// Create a new Agent tool with an executor
    #[must_use]
    pub fn new(executor: Arc<SubagentExecutor>) -> Self {
        Self {
            executor,
            workspace: None,
            session_provider: None,
            max_depth: DEFAULT_MAX_SPAWN_DEPTH,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    /// Create an Agent tool with an optional principal workspace.
    ///
    /// When the workspace is `Some`, `subagent_type` resolution will
    /// first look under `<workspace>/agents/<name>/...` before falling
    /// back to the global layout. Pass `None` for the legacy global-only
    /// lookup (standalone / test path).
    #[must_use]
    pub fn with_workspace(
        executor: Arc<SubagentExecutor>,
        workspace: Option<PathBuf>,
    ) -> Self {
        Self {
            executor,
            workspace,
            session_provider: None,
            max_depth: DEFAULT_MAX_SPAWN_DEPTH,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    /// Create an Agent tool with a session key provider
    #[must_use]
    pub fn with_session_provider(
        executor: Arc<SubagentExecutor>,
        provider: Box<dyn SessionKeyProvider>,
    ) -> Self {
        Self {
            executor,
            workspace: None,
            session_provider: Some(provider),
            max_depth: DEFAULT_MAX_SPAWN_DEPTH,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    /// Create an Agent tool with both a principal workspace and a session
    /// key provider. This is the production constructor used by the
    /// principal runner and the root agent.
    #[must_use]
    pub fn with_workspace_and_session(
        executor: Arc<SubagentExecutor>,
        workspace: Option<PathBuf>,
        provider: Box<dyn SessionKeyProvider>,
    ) -> Self {
        Self {
            executor,
            workspace,
            session_provider: Some(provider),
            max_depth: DEFAULT_MAX_SPAWN_DEPTH,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    /// Set maximum spawn depth
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set maximum concurrent runs
    #[must_use]
    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = max_concurrent;
        self
    }

    /// Get the executor
    #[must_use]
    pub fn executor(&self) -> &Arc<SubagentExecutor> {
        &self.executor
    }

    /// Resolve subagent_type to an AgentConfig, applying optional model override.
    async fn resolve_subagent_config(
        &self,
        subagent_type: &str,
        model_override: Option<&str>,
    ) -> anyhow::Result<AgentConfig> {
        // ADR-019/Track B: enforce the per-principal agent capability before
        // loading any on-disk config. If the executor carries a capability
        // snapshot, the requested subagent must be granted. If no snapshot is
        // registered (standalone / test path), fail-open to preserve existing
        // behavior.
        if let Some(caps) = self.executor.principal_capabilities() {
            let required = crate::extensions::framework::types::Capability::new(format!("agent:{subagent_type}"));
            if !caps.is_granted(&required) {
                anyhow::bail!(
                    "Subagent '{}' is not enabled for this principal. \
                     Grant 'agent:{}' and retry.",
                    subagent_type,
                    subagent_type
                );
            }
        }

        // Prefer a principal-scoped AGENT.md when a workspace is bound;
        // fall through to the global agents/ registry on miss.
        let config = if let Some(ref workspace) = self.workspace {
            match Self::resolve_principal_agent(subagent_type, workspace) {
                Ok(config) => config,
                Err(e) => {
                    tracing::debug!(
                        "Principal agent '{subagent_type}' not found in workspace '{}': {e}; falling back to global agent",
                        workspace.display()
                    );
                    Self::resolve_global_agent(subagent_type).await?
                }
            }
        } else {
            // Standalone / test path: resolve from the global layout only.
            Self::resolve_global_agent(subagent_type).await?
        };

        if let Some(model) = model_override {
            // **Track B**: per-agent `preferred_model_id` was removed.
            // The model override now flows through the subagent's
            // `provider_hint` parameter to `Agent::init_provider`
            // instead of mutating the on-disk agent config. We accept
            // and discard here so the parent path can keep its
            // current call shape; the override is applied at agent
            // construction time.
            let _ = model;
        }

        Ok(config)
    }

    /// Load a principal-scoped agent prompt from `<workspace>/agents/<name>/`.
    ///
    /// Supports the two on-disk shapes:
    /// - directory layout: `<workspace>/agents/<name>/AGENT.md`
    /// - flat layout: `<workspace>/agents/<name>.md`
    ///
    /// Returns an error if neither file exists.
    fn resolve_principal_agent(name: &str, workspace: &Path) -> anyhow::Result<AgentConfig> {
        let agents_dir = workspace.join("agents");
        let dir_layout = agents_dir.join(name).join("AGENT.md");
        let flat_layout = agents_dir.join(format!("{name}.md"));

        let agent_md = if dir_layout.exists() {
            dir_layout
        } else if flat_layout.exists() {
            flat_layout
        } else {
            anyhow::bail!(
                "No agent prompt found for principal agent '{name}' at {:?} or {:?}",
                dir_layout,
                flat_layout
            );
        };

        let prompt = crate::principal::agent_prompt::load_agent_prompt(&agent_md)
            .with_context(|| format!("Failed to load principal agent prompt '{name}'"))?;

        Ok(AgentConfig {
            name: prompt.name,
            description: prompt.frontmatter.description,
            prompt: Some(prompt.body),
            ..AgentConfig::default()
        })
    }

    /// Load an agent config from the global `~/.peko/agents/<name>/config.toml`
    /// layout.
    async fn resolve_global_agent(name: &str) -> anyhow::Result<AgentConfig> {
        let agent_name = parse_agent_name(name)?;
        let resolver = PathResolver::new();
        let config_path = resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Subagent type '{name}' not found at {config_path:?}");
        }
        let content = tokio::fs::read_to_string(&config_path).await?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse agent config for '{name}'"))
    }

    /// Execute subagent spawn in blocking mode (waits for completion, returns inline result)
    ///
    /// `ctx` is the parent tool's execution context. When `Some`, the
    /// abort signal is bridged into a `CancellationToken` (via
    /// [`crate::tools::bridge_to_cancellation_token`]) and forwarded
    /// to the sub-agent's `AgenticLoop` so a parent cancel propagates
    /// into a spawned sub-agent. The bridge guard is held for the
    /// duration of the spawn so the spawned task is aborted on drop.
    async fn execute_spawn_blocking(
        &self,
        prompt: &str,
        subagent_type: &str,
        isolated: bool,
        parent_session_key: &str,
        config: ExecutionConfig,
        description: Option<String>,
        cleanup: SpawnCleanupPolicy,
        ctx: Option<&ToolContext>,
    ) -> anyhow::Result<serde_json::Value> {
        let timeout_seconds = config.timeout_seconds;

        // Audit the spawn under the parent principal, if an observability hub
        // is attached to the executor. Failures are logged but do not block
        // the spawn.
        if let Some(obs) = self.executor.observability() {
            let details = serde_json::json!({
                "subagent_type": subagent_type,
                "principal_id": self.executor.principal_id().0,
                "principal_name": self.executor.principal_name(),
                "isolated": isolated,
                "cleanup": match cleanup {
                    SpawnCleanupPolicy::Keep => "keep",
                    SpawnCleanupPolicy::Delete => "delete",
                },
                "description": description,
                "parent_session_key": parent_session_key,
            });
            if let Err(e) = obs
                .audit("SubagentSpawn", self.executor.principal_name(), details)
                .await
            {
                tracing::warn!("Failed to audit subagent spawn: {e}");
            }
        }

        let (parent_cancel, _cancel_guard): (
            Option<tokio_util::sync::CancellationToken>,
            CancellationTokenBridgeGuard,
        ) = match ctx {
            Some(c) => {
                let (token, guard) = bridge_to_cancellation_token(Some(c.abort_signal()));
                (Some(token), guard)
            }
            None => (None, CancellationTokenBridgeGuard::noop()),
        };

        match self
            .executor
            .execute_and_wait(
                prompt,
                None,
                isolated,
                parent_session_key,
                config,
                timeout_seconds,
                parent_cancel,
            )
            .await
        {
            Ok(run) => {
                // Return inline result — the subagent's output is available immediately
                let status_str = run.status.as_str();
                let success = matches!(
                    run.status,
                    crate::extensions::framework::async_exec::executor::AsyncTaskStatus::Completed {
                        ..
                    }
                );

                let mut result = json!({
                    "status": status_str,
                    "run_id": run.run_id,
                    "child_session_key": run.child_session_key,
                    "success": success,
                    "subagent_type": subagent_type,
                    "description": description,
                    "isolated": isolated,
                    "timeout_seconds": timeout_seconds,
                    "cleanup": match cleanup {
                        SpawnCleanupPolicy::Keep => "keep",
                        SpawnCleanupPolicy::Delete => "delete",
                    }
                });

                // Include output or error if available
                if let Some(ref subagent_result) = run.result {
                    if let Some(ref output) = subagent_result.output {
                        result["output"] = json!(output);
                    }
                    if let Some(ref error) = subagent_result.error {
                        result["error"] = json!(error);
                    }
                }

                Ok(result)
            }
            Err(e) => Self::format_error_response(&e),
        }
    }

    /// Format error response
    ///
    /// Classifies the error using a typed [`SpawnError`] when available,
    /// falling back to string matching only for untyped errors.
    fn format_error_response(error: &anyhow::Error) -> anyhow::Result<serde_json::Value> {
        // Try typed classification first
        if let Some(spawn_err) = error.downcast_ref::<SpawnError>() {
            return match spawn_err {
                SpawnError::DepthLimitExceeded { .. } => Ok(json!({
                    "status": "forbidden",
                    "error": spawn_err.to_string(),
                    "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth."
                })),
                SpawnError::ConcurrentLimitExceeded { .. } => Ok(json!({
                    "status": "forbidden",
                    "error": spawn_err.to_string(),
                    "note": "Maximum concurrent subagent runs exceeded. Please wait for existing runs to complete."
                })),
                SpawnError::Timeout { .. } => Ok(json!({
                    "status": "timeout",
                    "error": spawn_err.to_string(),
                    "note": "Subagent execution timed out."
                })),
                SpawnError::ExecutionFailed(msg) => Ok(json!({
                    "status": "error",
                    "error": msg,
                })),
            };
        }

        // Fallback to string matching for untyped errors
        let error_msg = error.to_string();
        let lower_msg = error_msg.to_lowercase();
        if lower_msg.contains("depth") {
            Ok(json!({
                "status": "forbidden",
                "error": error_msg,
                "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth."
            }))
        } else if lower_msg.contains("concurrent") {
            Ok(json!({
                "status": "forbidden",
                "error": error_msg,
                "note": "Maximum concurrent subagent runs exceeded. Please wait for existing runs to complete."
            }))
        } else if lower_msg.contains("timeout") || lower_msg.contains("timed out") {
            Ok(json!({
                "status": "timeout",
                "error": error_msg,
                "note": "Subagent execution timed out."
            }))
        } else {
            Ok(json!({
                "status": "error",
                "error": error_msg
            }))
        }
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &'static str {
        "Agent"
    }

    fn description(&self) -> String {
        r#"Spawn a sub-agent run in an isolated or shared session.

The framework applies a constant 5-minute timeout to all tool calls. If the subagent takes longer than 5 minutes, the work is automatically detached to a background task and a receipt is returned.

Parameters:
- prompt: Description of the task to execute (required)
- subagent_type: Name of the agent config under ~/.peko/agents/<subagent_type>/config.toml (required)
- description: Optional description for tracking (matches Claude Code's Agent schema)
- model: Optional model override for the subagent (matches Claude Code's Agent schema)
- isolated: If true, creates isolated session without parent context (default: false)
- cleanup: "keep" or "delete" - what to do with session after completion (default: "keep")
- parent_session_key: Parent session key (optional - auto-detected if not provided)

Examples:
// Blocking spawn - parent waits for result (auto-detaches on timeout)
{"prompt": "Use Write to create report.txt with a summary", "subagent_type": "writer"}

// Isolated context - fresh session
{"prompt": "Analyze confidential data", "subagent_type": "analyst", "isolated": true, "cleanup": "delete"}"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Description of the task to execute"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "Name of the agent config under ~/.peko/agents/<subagent_type>/config.toml"
                },
                "description": {
                    "type": "string",
                    "description": "Optional description for tracking this spawn"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for the subagent"
                },
                "isolated": {
                    "type": "boolean",
                    "description": "If true, creates isolated session without parent context",
                    "default": false
                },
                "cleanup": {
                    "type": "string",
                    "description": "What to do with session after completion: 'keep' or 'delete'",
                    "default": "keep"
                },
                "parent_session_key": {
                    "type": "string",
                    "description": "Parent session key (auto-detected if not provided)"
                }
            },
            "required": ["prompt", "subagent_type"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: AgentArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let cleanup = args.cleanup.map_or(SpawnCleanupPolicy::Keep, |s| {
            match s.to_lowercase().as_str() {
                "delete" => SpawnCleanupPolicy::Delete,
                _ => SpawnCleanupPolicy::Keep,
            }
        });

        // Get parent session key - from params or session provider
        let parent_session_key = if let Some(key) = args.parent_session_key {
            key
        } else if let Some(ref provider) = self.session_provider {
            provider.current_session_key()
        } else {
            return Err(anyhow::anyhow!(
                "Agent tool requires a parent_session_key parameter or session provider. \
                Please provide parent_session_key in the tool parameters."
            ));
        };

        // Resolve subagent_type to a concrete agent config and apply model override.
        let subagent_config = self
            .resolve_subagent_config(&args.subagent_type, args.model.as_deref())
            .await?;

        // Update the executor with the resolved subagent config so the spawned
        // session uses the subagent's provider/model wiring.
        let executor = self
            .executor
            .as_ref()
            .clone()
            .with_agent_config(subagent_config);

        let description = args.description;

        // Build execution config with defaults
        let config = ExecutionConfig {
            timeout_seconds: 300, // 5-min default; the framework auto-detaches on timeout
            cleanup,
            label: description.clone(),
            announce_completion: true,
            max_depth: self.max_depth,
        };

        // Always go through the blocking path; the framework detaches on
        // timeout. If the caller wants explicit async, they invoke this
        // tool via AsyncSpawn.
        let tool = AgentTool {
            executor: Arc::new(executor),
            workspace: self.workspace.clone(),
            session_provider: None,
            max_depth: self.max_depth,
            max_concurrent: self.max_concurrent,
        };
        tool.execute_spawn_blocking(
            &args.prompt,
            &args.subagent_type,
            args.isolated,
            &parent_session_key,
            config,
            description,
            cleanup,
            None,
        )
        .await
    }

    /// Override the trait default to bridge the abort signal from
    /// `ToolContext` into a `CancellationToken` for the sub-agent.
    /// The default `Tool::execute_with_context` would call `self.execute`
    /// directly, losing the cancel signal. We re-parse `params` and
    /// dispatch to `execute_spawn_blocking(Some(ctx))` so the sub-agent
    /// observes the parent's cancel at iteration boundaries.
    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let args: AgentArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let cleanup = args.cleanup.map_or(SpawnCleanupPolicy::Keep, |s| {
            match s.to_lowercase().as_str() {
                "delete" => SpawnCleanupPolicy::Delete,
                _ => SpawnCleanupPolicy::Keep,
            }
        });

        let parent_session_key = if let Some(key) = args.parent_session_key {
            key
        } else if let Some(ref provider) = self.session_provider {
            provider.current_session_key()
        } else {
            return Err(anyhow::anyhow!(
                "Agent tool requires a parent_session_key parameter or session provider."
            ));
        };

        let subagent_config = self
            .resolve_subagent_config(&args.subagent_type, args.model.as_deref())
            .await?;

        let executor = self
            .executor
            .as_ref()
            .clone()
            .with_agent_config(subagent_config);

        let tool = AgentTool {
            executor: Arc::new(executor),
            workspace: self.workspace.clone(),
            session_provider: None,
            max_depth: self.max_depth,
            max_concurrent: self.max_concurrent,
        };
        let config = ExecutionConfig {
            timeout_seconds: 300,
            cleanup,
            label: args.description.clone(),
            announce_completion: true,
            max_depth: self.max_depth,
        };
        tool.execute_spawn_blocking(
            &args.prompt,
            &args.subagent_type,
            args.isolated,
            &parent_session_key,
            config,
            args.description,
            cleanup,
            Some(ctx),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::types::Capabilities;
    use crate::subject::PrincipalId;
    use crate::session::manager::SessionManager;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    async fn temp_agent_workspace(name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agents").join(name);
        tokio::fs::create_dir_all(&agent_dir).await.unwrap();
        tokio::fs::write(
            agent_dir.join("AGENT.md"),
            format!("---\nname: {}\ndescription: test\n---\n\ntest", name),
        )
        .await
        .unwrap();
        dir
    }

    fn test_executor(
        pid: PrincipalId,
        capabilities: Option<Arc<Capabilities>>,
    ) -> Arc<SubagentExecutor> {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        Arc::new(
            SubagentExecutor::new(manager, "test_agent", 5, pid)
                .with_principal_capabilities(capabilities),
        )
    }

    #[tokio::test]
    async fn test_agent_state_registry_allows_enabled_subagent() {
        let pid = PrincipalId::generate();
        let workspace = temp_agent_workspace("writer").await;
        let caps = Arc::new(Capabilities::with_grants(["agent:writer"]));
        let tool = AgentTool::with_workspace(
            test_executor(pid.clone(), Some(caps)),
            Some(workspace.path().to_path_buf()),
        );

        let result = tool.resolve_subagent_config("writer", None).await;
        assert!(
            result.is_ok(),
            "enabled subagent should resolve: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_agent_state_registry_denies_disabled_subagent() {
        let pid = PrincipalId::generate();
        let workspace = temp_agent_workspace("writer").await;
        let caps = Arc::new(Capabilities::with_grants(["agent:other"]));
        let tool = AgentTool::with_workspace(
            test_executor(pid.clone(), Some(caps)),
            Some(workspace.path().to_path_buf()),
        );

        let result = tool.resolve_subagent_config("writer", None).await;
        assert!(
            result.is_err(),
            "disabled subagent should be rejected by capability snapshot"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not enabled"),
            "error should explain allowlist denial: {err}"
        );
    }

    #[tokio::test]
    async fn test_agent_state_registry_unregistered_principal_is_fail_open() {
        let pid = PrincipalId::generate();
        let workspace = temp_agent_workspace("writer").await;
        let tool = AgentTool::with_workspace(
            test_executor(pid, None),
            Some(workspace.path().to_path_buf()),
        );

        // No capability snapshot registered for this principal; standalone/test
        // path should remain usable.
        let result = tool.resolve_subagent_config("writer", None).await;
        assert!(
            result.is_ok(),
            "unregistered principal should fail-open: {:?}",
            result.err()
        );
    }

    /// Migrated from `agent_service.rs::test_resolve_principal_agent_flat_file`.
    ///
    /// Pins the flat-file shape (`<workspace>/agents/<name>.md`).
    #[tokio::test]
    async fn test_agent_tool_resolves_principal_agent_flat_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = temp_dir.path().join("workspace");
        let agents_dir = workspace.join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        std::fs::write(
            agents_dir.join("worker.md"),
            "---\nname: Worker Bot\ndescription: A flat-file worker agent\n---\n\nYou are a worker.\n",
        )
        .unwrap();

        let tool = AgentTool::with_workspace(
            test_executor(PrincipalId::generate(), None),
            Some(workspace.clone()),
        );

        let config = tool.resolve_subagent_config("worker", None).await.unwrap();

        assert_eq!(config.name, "Worker Bot");
        assert_eq!(
            config.description.as_deref(),
            Some("A flat-file worker agent")
        );
        assert!(config
            .prompt
            .as_deref()
            .unwrap()
            .contains("You are a worker."));
    }

    /// Migrated from `agent_service.rs::test_resolve_principal_agent_directory_layout`.
    ///
    /// Pins the directory shape (`<workspace>/agents/<name>/AGENT.md`).
    #[tokio::test]
    async fn test_agent_tool_resolves_principal_agent_directory_layout() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = temp_dir.path().join("workspace");
        let agent_dir = workspace.join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();

        std::fs::write(
            agent_dir.join("AGENT.md"),
            "---\nname: Worker Bot\ndescription: A directory-layout worker agent\n---\n\nYou are a worker.\n",
        )
        .unwrap();

        let tool = AgentTool::with_workspace(
            test_executor(PrincipalId::generate(), None),
            Some(workspace.clone()),
        );

        let config = tool.resolve_subagent_config("worker", None).await.unwrap();

        assert_eq!(config.name, "Worker Bot");
        assert_eq!(
            config.description.as_deref(),
            Some("A directory-layout worker agent")
        );
    }

    /// New test covering the workspace-not-found → global-fallback path.
    ///
    /// When a workspace is bound but contains no AGENT.md for the requested
    /// subagent, resolution must log a debug message and fall through to the
    /// global `{PEKO_HOME}/agents/<name>/config.toml` layout. We sandbox
    /// with the `PEKO_HOME` environment variable (consumed by
    /// `PathResolver::default_config_dir`) so the global lookup resolves to a
    /// test-controlled directory.
    #[tokio::test]
    async fn test_agent_tool_falls_back_to_global_when_principal_missing() {
        // Sandbox a temp PEKO_HOME so PathResolver resolves under it.
        let peko_home = tempfile::tempdir().unwrap();
        let prev_peko_home = std::env::var_os("PEKO_HOME");
        // SAFETY: tests run single-threaded for the env var window.
        unsafe { std::env::set_var("PEKO_HOME", peko_home.path()) };

        let global_agents_dir =
            peko_home.path().join("agents").join("fallback-agent");
        std::fs::create_dir_all(&global_agents_dir).unwrap();
        std::fs::write(
            global_agents_dir.join("config.toml"),
            r#"
name = "Fallback Agent"
description = "Global fallback for missing principal agent"

[model]
provider = "openai"
model_id = "gpt-4o"
"#,
        )
        .unwrap();

        // Workspace with NO matching AGENT.md — directory exists but is empty.
        let workspace = tempfile::tempdir().unwrap().path().to_path_buf();
        std::fs::create_dir_all(workspace.join("agents")).unwrap();

        let tool = AgentTool::with_workspace(
            test_executor(PrincipalId::generate(), None),
            Some(workspace),
        );

        let result = tool.resolve_subagent_config("fallback-agent", None).await;

        // Restore PEKO_HOME before asserting so a panic in assert! doesn't
        // leak state into other tests.
        unsafe {
            match prev_peko_home {
                Some(v) => std::env::set_var("PEKO_HOME", v),
                None => std::env::remove_var("PEKO_HOME"),
            }
        }

        let config = result.unwrap_or_else(|e| panic!("expected global fallback to succeed: {e:?}"));
        assert_eq!(config.name, "Fallback Agent");
        assert_eq!(config.description.as_deref(), Some("Global fallback for missing principal agent"));
    }

    /// Companion to the fallback test: when the workspace is bound but no
    /// global agent exists either, resolution fails with a clear message.
    #[tokio::test]
    async fn test_agent_tool_errors_when_neither_principal_nor_global_exists() {
        let workspace = tempfile::tempdir().unwrap().path().to_path_buf();
        std::fs::create_dir_all(workspace.join("agents")).unwrap();

        let tool = AgentTool::with_workspace(
            test_executor(PrincipalId::generate(), None),
            Some(workspace),
        );

        let err = tool
            .resolve_subagent_config("definitely-not-registered", None)
            .await
            .expect_err("expected resolution to fail when neither layer has the agent");
        let msg = err.to_string();
        assert!(
            msg.contains("not found"),
            "expected 'not found' in error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_agent_tool_creation() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = Arc::new(SubagentExecutor::new(
            manager,
            "test_agent",
            5,
            crate::subject::PrincipalId::generate(),
        ));
        let tool = AgentTool::new(executor);

        assert_eq!(tool.name(), "Agent");
    }

    #[tokio::test]
    async fn test_agent_tool_with_session_provider() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = Arc::new(SubagentExecutor::new(
            manager,
            "test_agent",
            5,
            crate::subject::PrincipalId::generate(),
        ));

        let provider = Box::new(StaticSessionKeyProvider::new("test:session:key"));
        let tool = AgentTool::with_session_provider(executor, provider);

        assert_eq!(tool.name(), "Agent");
    }

    #[test]
    fn test_default_max_depth() {
        assert_eq!(DEFAULT_MAX_SPAWN_DEPTH, 3);
    }

    #[test]
    fn test_default_max_concurrent() {
        assert_eq!(DEFAULT_MAX_CONCURRENT, 5);
    }

    #[tokio::test]
    async fn test_error_response_formatting() {
        // Test typed depth error
        let depth_err = anyhow::anyhow!(SpawnError::DepthLimitExceeded { current: 4, max: 3 });
        let response = AgentTool::format_error_response(&depth_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "forbidden");
        assert!(response["note"].as_str().unwrap().contains("depth"));
        assert!(response["error"].as_str().unwrap().contains('4'));

        // Test typed concurrent error
        let concurrent_err =
            anyhow::anyhow!(SpawnError::ConcurrentLimitExceeded { current: 5, max: 5 });
        let response = AgentTool::format_error_response(&concurrent_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "forbidden");
        assert!(response["note"].as_str().unwrap().contains("concurrent"));

        // Test typed timeout error
        let timeout_err = anyhow::anyhow!(SpawnError::Timeout { seconds: 30 });
        let response = AgentTool::format_error_response(&timeout_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "timeout");
        assert!(response["error"].as_str().unwrap().contains("30"));

        // Test typed execution failed error
        let exec_err = anyhow::anyhow!(SpawnError::ExecutionFailed(
            "something went wrong".to_string()
        ));
        let response = AgentTool::format_error_response(&exec_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "error");
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("something went wrong"));

        // Test fallback string matching for untyped errors
        let untyped = anyhow::anyhow!("Some random depth-related failure");
        let response = AgentTool::format_error_response(&untyped).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "forbidden");
        assert!(response["note"].as_str().unwrap().contains("depth"));
    }

    #[test]
    fn test_args_parsing() {
        let json = r#"{
            "prompt": "Do something",
            "subagent_type": "writer",
            "description": "my-task",
            "isolated": true,
            "cleanup": "delete"
        }"#;

        let args: AgentArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.prompt, "Do something");
        assert_eq!(args.subagent_type, "writer");
        assert_eq!(args.description, Some("my-task".to_string()));
        assert!(args.isolated);
        assert_eq!(args.cleanup, Some("delete".to_string()));
    }
}
