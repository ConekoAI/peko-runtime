//! Agent tool (Claude Code parity) — Phase 10e.
//!
//! Spawns subagent sessions for isolated task execution. Results are
//! announced back to the parent via the event system.
//!
//! Note: Async execution and timeout are handled by the framework-level
//! `AsyncExecutionRouter` using a constant 5-minute timeout. On timeout,
//! the work is detached to a background task automatically.
//!
//! The tool itself is a thin shell over [`SubagentRuntime`]. Disk I/O
//! (`PathResolver`, `principal::agent_prompt`), capability checks,
//! observability audit, and the actual `SubagentExecutor::execute_and_wait`
//! call all live behind the port — see
//! `src/agents/subagent_runtime_impl.rs` for the production adapter.

use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::messaging::dto::{ExecutionConfig, SpawnCleanupPolicy, SpawnError};
use crate::messaging::subagent_runtime::{
    SharedSubagentRuntime, SpawnAuditEvent, SpawnRequest, SubagentRuntime,
};

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

// Blanket impl so callers can store `Arc<DynamicSessionKeyProvider>`
// (the runtime mutable session-key handle owned by the daemon) and
// pass the Arc directly where a `Box<dyn SessionKeyProvider>` is
// expected. The orphan rule permits this blanket impl because
// `SessionKeyProvider` is local to `peko_tools_builtin`.
impl<T: SessionKeyProvider + ?Sized> SessionKeyProvider for std::sync::Arc<T> {
    fn current_session_key(&self) -> String {
        (**self).current_session_key()
    }
}

// Note: `DynamicSessionKeyProvider` (and the
// `impl SessionKeyProvider for Arc<DynamicSessionKeyProvider>` shim)
// are intentionally not lifted — they belong to the daemon/runtime
// layer that needs to mutate session keys at runtime, not to the
// built-in tool itself. Root continues to define them at
// `src/tools/builtin/messaging/agent.rs` (now a shim) and the
// principal runner constructs the Arc to pass into AgentTool.

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
    /// Runtime port — the only seam between the tool and the
    /// daemon/agent state.
    runtime: SharedSubagentRuntime,
    /// Optional principal workspace. When set, `subagent_type` resolution
    /// prefers principal-scoped `AGENT.md` files at
    /// `<workspace>/agents/<name>/...` before falling back to the global
    /// `~/.peko/agents/<name>/config.toml` layout.
    workspace: Option<PathBuf>,
    /// Session key provider to get current session at execution time.
    session_provider: Option<Box<dyn SessionKeyProvider>>,
    /// Maximum spawn depth allowed
    max_depth: u32,
    /// Maximum concurrent runs
    max_concurrent: usize,
}

impl AgentTool {
    /// Create a new Agent tool with a runtime port.
    #[must_use]
    pub fn new(runtime: SharedSubagentRuntime) -> Self {
        Self {
            runtime,
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
    pub fn with_workspace(runtime: SharedSubagentRuntime, workspace: Option<PathBuf>) -> Self {
        Self {
            runtime,
            workspace,
            session_provider: None,
            max_depth: DEFAULT_MAX_SPAWN_DEPTH,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    /// Create an Agent tool with a session key provider
    #[must_use]
    pub fn with_session_provider(
        runtime: SharedSubagentRuntime,
        provider: Box<dyn SessionKeyProvider>,
    ) -> Self {
        Self {
            runtime,
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
        runtime: SharedSubagentRuntime,
        workspace: Option<PathBuf>,
        provider: Box<dyn SessionKeyProvider>,
    ) -> Self {
        Self {
            runtime,
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

    /// Resolve subagent_type to an AgentConfig via the runtime port.
    async fn resolve_subagent_config(
        &self,
        subagent_type: &str,
        model_override: Option<&str>,
    ) -> anyhow::Result<crate::messaging::dto::AgentConfig> {
        // ADR-019/Track B: enforce the per-principal agent capability before
        // loading any on-disk config. If the runtime's underlying executor
        // carries a capability snapshot, the requested subagent must be
        // granted. If no snapshot is registered (standalone / test path),
        // fail-open to preserve existing behavior.
        if !self.runtime.is_subagent_enabled(subagent_type) {
            anyhow::bail!(
                "Subagent '{subagent_type}' is not enabled for this principal. \
                 Grant 'agent:{subagent_type}' and retry."
            );
        }

        self.runtime
            .resolve_agent_config(subagent_type, self.workspace.as_deref(), model_override)
            .await
    }

    /// Execute subagent spawn in blocking mode (waits for completion, returns inline result)
    ///
    /// `ctx` is the parent tool's execution context. When `Some`, the
    /// abort signal is bridged into a `CancellationToken` (via
    /// [`peko_tools_core::bridge_to_cancellation_token`]) and
    /// forwarded to the sub-agent's `AgenticLoop` so a parent cancel
    /// propagates into a spawned sub-agent. The bridge guard is held
    /// for the duration of the spawn so the spawned task is aborted on
    /// drop.
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

        // Resolve the subagent config first so we can audit with
        // the resolved name (and so `audit_spawn` runs even when the
        // spawn is later blocked by a runtime error).
        let subagent_config = self
            .runtime
            .resolve_agent_config(subagent_type, self.workspace.as_deref(), None)
            .await?;

        // Audit the spawn under the parent principal, if an observability hub
        // is attached to the runtime. Failures are logged but do not block
        // the spawn.
        let principal_id = self.runtime.principal_id();
        self.runtime
            .audit_spawn(SpawnAuditEvent {
                subagent_type: subagent_type.to_string(),
                principal_id: principal_id.clone(),
                principal_name: self.runtime.principal_name(),
                isolated,
                cleanup,
                description: description.clone(),
                parent_session_key: parent_session_key.to_string(),
            })
            .await;

        let (parent_cancel, _cancel_guard): (
            Option<tokio_util::sync::CancellationToken>,
            peko_tools_core::CancellationTokenBridgeGuard,
        ) = match ctx {
            Some(c) => {
                let (token, guard) =
                    peko_tools_core::bridge_to_cancellation_token(Some(c.abort_signal()));
                (Some(token), guard)
            }
            None => (None, peko_tools_core::CancellationTokenBridgeGuard::noop()),
        };

        match self
            .runtime
            .execute_and_wait(SpawnRequest {
                prompt: prompt.to_string(),
                subagent_type: subagent_type.to_string(),
                isolated,
                parent_session_key: parent_session_key.to_string(),
                config: config.clone(),
                timeout_seconds,
                parent_cancel,
                subagent_config,
            })
            .await
        {
            Ok(run) => {
                let status_str = run.status.as_str();
                let success = matches!(
                    run.status,
                    peko_extension_api::AsyncTaskStatus::Completed { .. }
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
                    "cleanup": cleanup.as_str(),
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

// (Trait helpers used by the port live in `subagent_runtime.rs`:
// `SubagentRuntimeAuditExt` provides principal-id/name accessors that
// `AgentTool` uses when building a `SpawnAuditEvent`. Test fixtures
// override the defaults; the production `SubagentExecutorRuntime`
// adapter overrides them with real principal state.)

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
        self.execute_spawn_blocking(
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

        let config = ExecutionConfig {
            timeout_seconds: 300,
            cleanup,
            label: args.description.clone(),
            announce_completion: true,
            max_depth: self.max_depth,
        };
        self.execute_spawn_blocking(
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

// ─── Test fixture: `TestSubagentRuntime` ──────────────────────────

/// In-memory [`SubagentRuntime`] for tests. Mirrors the production
/// `SubagentExecutor` semantics: capability snapshot, workspace +
/// global agent resolution, audit log capture, and a stubbed
/// `execute_and_wait` that returns a configurable run view.
#[cfg(test)]
pub struct TestSubagentRuntime {
    inner: std::sync::Mutex<TestSubagentState>,
}

#[cfg(test)]
struct TestSubagentState {
    /// Capability grants (mirrors `Capabilities::with_grants`).
    grants: Vec<String>,
    /// Registered agent configs by name.
    configs: std::collections::HashMap<String, crate::messaging::dto::AgentConfig>,
    /// Audit log of spawn events.
    audits: Vec<SpawnAuditEvent>,
    /// Whether `execute_and_wait` should succeed (true) or fail with
    /// an error (false).
    succeed_on_execute: bool,
    /// Principal id used in audit events.
    principal_id: String,
    /// Principal display name used in audit events.
    principal_name: Option<String>,
}

#[cfg(test)]
impl TestSubagentRuntime {
    /// Build an empty test runtime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(TestSubagentState {
                grants: Vec::new(),
                configs: std::collections::HashMap::new(),
                audits: Vec::new(),
                succeed_on_execute: true,
                principal_id: String::new(),
                principal_name: None,
            }),
        }
    }

    /// Register a capability grant (e.g. `"agent:writer"`).
    pub fn grant(&self, capability: impl Into<String>) {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .grants
            .push(capability.into());
    }

    /// Register an agent config (keyed by name).
    pub fn register_agent(
        &self,
        name: impl Into<String>,
        config: crate::messaging::dto::AgentConfig,
    ) {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .configs
            .insert(name.into(), config);
    }

    /// Get the audit log (cloned).
    #[must_use]
    pub fn audits(&self) -> Vec<SpawnAuditEvent> {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .audits
            .clone()
    }

    /// Whether the runtime should succeed on `execute_and_wait`.
    pub fn set_succeed_on_execute(&self, succeed: bool) {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .succeed_on_execute = succeed;
    }

    /// Set the principal id used for audit events.
    pub fn set_principal_id(&self, id: impl Into<String>) {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .principal_id = id.into();
    }
}

#[cfg(test)]
impl Default for TestSubagentRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[async_trait]
impl SubagentRuntime for TestSubagentRuntime {
    fn is_subagent_enabled(&self, subagent_type: &str) -> bool {
        let state = self
            .inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned");
        // Fail-open when no grants registered (standalone/test path).
        if state.grants.is_empty() {
            return true;
        }
        let required = format!("agent:{subagent_type}");
        state.grants.iter().any(|g| g == &required)
    }

    async fn resolve_agent_config(
        &self,
        name: &str,
        _workspace: Option<&Path>,
        _model_override: Option<&str>,
    ) -> anyhow::Result<crate::messaging::dto::AgentConfig> {
        let state = self
            .inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned");
        state
            .configs
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Subagent type '{name}' not registered"))
    }

    async fn audit_spawn(&self, event: SpawnAuditEvent) {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .audits
            .push(event);
    }

    fn principal_id(&self) -> String {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .principal_id
            .clone()
    }

    fn principal_name(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .principal_name
            .clone()
    }

    async fn execute_and_wait(
        &self,
        request: SpawnRequest,
    ) -> anyhow::Result<crate::messaging::dto::SubagentRunView> {
        let succeed = self
            .inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .succeed_on_execute;
        if !succeed {
            return Err(anyhow::anyhow!("test failure"));
        }
        Ok(crate::messaging::dto::SubagentRunView {
            run_id: "test-run".into(),
            child_session_key: "test-child".into(),
            parent_session_key: request.parent_session_key.clone(),
            task: request.prompt.clone(),
            status: peko_extension_api::AsyncTaskStatus::Completed {
                result: peko_tools_core::ToolResult::success(serde_json::json!("test")),
            },
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            cleanup: request.config.cleanup,
            label: request.config.label,
            result: None,
            depth: request.config.max_depth,
            announce_completion: request.config.announce_completion,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messaging::dto::AgentConfig;

    #[tokio::test]
    async fn test_agent_state_registry_allows_enabled_subagent() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.grant("agent:writer");
        runtime.register_agent(
            "writer",
            AgentConfig {
                name: "writer".into(),
                description: Some("writer agent".into()),
                ..Default::default()
            },
        );
        let tool = AgentTool::with_workspace(
            runtime.clone() as SharedSubagentRuntime,
            Some(PathBuf::from("/tmp/nonexistent")),
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
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.grant("agent:other");
        let tool = AgentTool::with_workspace(
            runtime.clone() as SharedSubagentRuntime,
            Some(PathBuf::from("/tmp/nonexistent")),
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
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.register_agent(
            "writer",
            AgentConfig {
                name: "writer".into(),
                ..Default::default()
            },
        );
        let tool = AgentTool::with_workspace(
            runtime.clone() as SharedSubagentRuntime,
            Some(PathBuf::from("/tmp/nonexistent")),
        );

        // No grants registered; standalone/test path should fail-open.
        let result = tool.resolve_subagent_config("writer", None).await;
        assert!(
            result.is_ok(),
            "unregistered principal should fail-open: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_agent_tool_creation() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

        assert_eq!(tool.name(), "Agent");
    }

    #[tokio::test]
    async fn test_agent_tool_with_session_provider() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let provider = Box::new(StaticSessionKeyProvider::new("test:session:key"));
        let tool =
            AgentTool::with_session_provider(runtime.clone() as SharedSubagentRuntime, provider);

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

    #[tokio::test]
    async fn test_audit_records_spawn_event() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.grant("agent:writer");
        runtime.register_agent(
            "writer",
            AgentConfig {
                name: "writer".into(),
                ..Default::default()
            },
        );
        runtime.set_principal_id("test-principal");

        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);
        let _ = tool.resolve_subagent_config("writer", None).await.unwrap();

        // Audit log should be populated by `execute_spawn_blocking` — we
        // don't call it here, but resolve_subagent_config shouldn't add
        // anything. Re-test through execute_spawn_blocking:
        let result = tool
            .execute_spawn_blocking(
                "do work",
                "writer",
                false,
                "parent:1",
                ExecutionConfig {
                    timeout_seconds: 60,
                    cleanup: SpawnCleanupPolicy::Keep,
                    label: None,
                    announce_completion: true,
                    max_depth: 3,
                },
                Some("my-task".into()),
                SpawnCleanupPolicy::Keep,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result["status"], "completed");
        assert_eq!(result["subagent_type"], "writer");

        let audits = runtime.audits();
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].subagent_type, "writer");
        assert_eq!(audits[0].principal_id, "test-principal");
        assert_eq!(audits[0].parent_session_key, "parent:1");
    }
}
