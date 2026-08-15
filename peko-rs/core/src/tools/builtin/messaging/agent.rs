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
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;

use crate::tools::builtin::messaging::dto::{ExecutionConfig, SpawnCleanupPolicy, SpawnError};
#[cfg(test)]
use crate::tools::builtin::messaging::subagent_runtime::SubagentRuntime;
use crate::tools::builtin::messaging::subagent_runtime::{
    SharedSubagentRuntime, SpawnAuditEvent, SpawnRequest,
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
    /// Action to perform: "new" (default), "resume", or "compact".
    /// Per-action parameter requirements are validated in code, not in
    /// the JSON schema (`new`: prompt+subagent_type; `resume`:
    /// session_key+prompt+subagent_type; `compact`: session_key only).
    #[serde(default = "default_action")]
    pub action: String,
    /// Task description / prompt for the subagent (required for `new`
    /// and `resume`; ignored for `compact`).
    #[serde(default)]
    pub prompt: String,
    /// Subagent type: name of the agent config under ~/.peko/agents/<subagent_type>/config.toml
    /// (required for `new` and `resume`; ignored for `compact`).
    #[serde(default)]
    pub subagent_type: String,
    /// Target session id for `resume` and `compact` (ignored for `new`).
    #[serde(default)]
    pub session_key: Option<String>,
    /// Optional description for tracking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional slug for the spawned session (action `new` only) — the
    /// per-parent-unique path segment for `/a/b` addressing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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

/// Serde default for [`AgentArgs::action`]: the historical behavior is
/// a fresh spawn, so an omitted action means `new`.
fn default_action() -> String {
    "new".to_string()
}

/// The Agent tool's three actions (round-7 action surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentAction {
    /// Spawn a fresh subagent session (default).
    New,
    /// Re-attach a run to an existing spawned session.
    Resume,
    /// Flag a session for engine-driven compaction at its next run.
    Compact,
}

/// Parse the `action` parameter. Unknown values are a structured
/// validation error naming the valid set.
fn parse_action(raw: &str) -> anyhow::Result<AgentAction> {
    match raw {
        "new" => Ok(AgentAction::New),
        "resume" => Ok(AgentAction::Resume),
        "compact" => Ok(AgentAction::Compact),
        other => Err(anyhow::anyhow!(
            "unknown action '{other}' — valid actions: \"new\", \"resume\", \"compact\""
        )),
    }
}

/// Parse the `cleanup` parameter via [`SpawnCleanupPolicy::from_str`].
/// Unknown values are a structured validation error (previously they
/// silently became `Keep` — round-7 audit fix).
fn parse_cleanup(cleanup: Option<&str>) -> anyhow::Result<SpawnCleanupPolicy> {
    match cleanup {
        None => Ok(SpawnCleanupPolicy::Keep),
        Some(s) => SpawnCleanupPolicy::from_str(s).ok_or_else(|| {
            anyhow::anyhow!("invalid cleanup '{s}' — valid values: \"keep\", \"delete\"")
        }),
    }
}

/// Per-action parameter requirements. The JSON schema's `required`
/// list is intentionally empty — requirements depend on `action`, so
/// they are validated here instead of in the schema.
fn validate_action_args(action: AgentAction, args: &AgentArgs) -> anyhow::Result<()> {
    match action {
        AgentAction::New => {
            if args.prompt.is_empty() || args.subagent_type.is_empty() {
                return Err(anyhow::anyhow!(
                    "action \"new\" requires 'prompt' and 'subagent_type'"
                ));
            }
        }
        AgentAction::Resume => {
            if args.session_key.is_none() {
                return Err(anyhow::anyhow!(
                    "action \"resume\" requires 'session_key' — pass a session id from the \
                     session tool's list"
                ));
            }
            if args.prompt.is_empty() || args.subagent_type.is_empty() {
                return Err(anyhow::anyhow!(
                    "action \"resume\" requires 'prompt' and 'subagent_type'"
                ));
            }
            if args.isolated {
                return Err(anyhow::anyhow!(
                    "'action \"resume\"' and 'isolated: true' are contradictory — resume \
                     re-attaches to an existing session's history while isolated starts a \
                     fresh context"
                ));
            }
        }
        AgentAction::Compact => {
            if args.session_key.is_none() {
                return Err(anyhow::anyhow!(
                    "action \"compact\" requires 'session_key' — pass a session id from the \
                     session tool's list"
                ));
            }
        }
    }
    Ok(())
}

/// Map the action surface onto the internal spawn request: `resume`
/// carries its `session_key` as the re-attach target; `new` (and the
/// already-dispatched `compact`) carry none.
fn resume_target(action: AgentAction, args: &AgentArgs) -> Option<String> {
    if action == AgentAction::Resume {
        args.session_key.clone()
    } else {
        None
    }
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
    ) -> anyhow::Result<crate::tools::builtin::messaging::dto::AgentConfig> {
        // ADR-019/Track B: enforce the per-principal agent capability before
        // loading any on-disk config. Missing authorization context and missing
        // grants are both denied.
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
        // Phase 1: the parent-driven model id (if any). Surfaces on
        // the audit row (`model_id`) and gets threaded onto
        // `SpawnRequest.model` so the adapter can project it onto
        // the root-side `ExecutionConfig.model_override`.
        model: Option<String>,
        // Phase 5b: re-attach to an existing spawned session instead
        // of spawning a fresh one (mutually exclusive with `isolated`,
        // validated by the callers). The tool maps `action = "resume"`
        // + `session_key` onto this field.
        resume_session: Option<String>,
        // Agent tool `name` param: slug for the spawned session's
        // metadata (ignored on the resume path — the session already
        // exists).
        name: Option<String>,
        // The caller's own current session id — the adapter uses it
        // to ownership-validate an explicit `parent_session_key`.
        caller_session_key: Option<String>,
        ctx: Option<&ToolContext>,
    ) -> anyhow::Result<serde_json::Value> {
        let timeout_seconds = config.timeout_seconds;

        // Resolve the subagent config first so we can audit with
        // the resolved name (and so `audit_spawn` runs even when the
        // spawn is later blocked by a runtime error).
        let subagent_config = self
            .runtime
            .resolve_agent_config(subagent_type, self.workspace.as_deref(), model.as_deref())
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
                // Phase 1: parent-driven model choice (when set).
                // `None` means the child inherits the parent's
                // model — pre-Phase-1 behavior.
                model_id: model.clone(),
                // Phase 3 — conservative cost estimate from the
                // chosen model's `PricingHint` and the 4K-in +
                // 1K-out token projection. `None` when the
                // principal has no `cost_per_call_max` or the
                // model carries no pricing hint. The runtime
                // knows what model is being spawned; the cost
                // estimator is centralised on the runtime
                // adapter so Phase 1's model resolution can
                // populate this without touching this call
                // site.
                cost_estimate_usd: self.runtime.spawn_cost_estimate_usd(),
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
                // Phase 1: forward the model override. Adapter
                // lifts it onto `ExecutionConfig.model_override`
                // before calling `execute_subagent_task`.
                model: model.clone(),
                resume_session,
                name,
                caller_session_key,
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
    /// falling back to string matching only for untyped errors. Walks
    /// the `anyhow` chain first; if no typed error is found (the async
    /// exec layer stringifies the error at `executor.rs:343` before it
    /// reaches us), parses the well-defined `SpawnError` Display
    /// format to reconstruct the typed fields.
    fn format_error_response(error: &anyhow::Error) -> anyhow::Result<serde_json::Value> {
        // 1. Try typed classification first, walking the anyhow chain
        //    because intermediate layers re-wrap the typed error with
        //    a string-formatted `anyhow!`.
        for source in error.chain() {
            if let Some(spawn_err) = source.downcast_ref::<SpawnError>() {
                return Self::spawn_error_to_json(spawn_err);
            }
        }

        // 2. The async-exec layer at `extensions/async_exec/executor.rs:343`
        //    stringifies `e.to_string()` when constructing
        //    `AsyncTaskStatus::Failed`. The typed chain is gone by the
        //    time we get here, so parse the well-defined
        //    `SpawnError::Display` shape and reconstruct the typed
        //    fields. Display formats (canonical from
        //    `subagent_error.rs` and `messaging/dto.rs`):
        //      DepthLimitExceeded { current, max }
        //        → "Maximum spawn depth exceeded: {current} (max: {max})"
        //      ConcurrentLimitExceeded { current, max }
        //        → "Maximum concurrent subagent runs exceeded: {current} (max: {max})"
        //      Timeout { seconds }
        //        → "Subagent execution timed out after {seconds}s"
        let error_msg = error.to_string();

        if let Some((current, max)) =
            parse_two_u32s(&error_msg, "Maximum spawn depth exceeded:", "(max:")
        {
            return Ok(json!({
                "status": "forbidden",
                "error_type": "DepthLimitExceeded",
                "current_depth": current,
                "max_depth": max,
                "error": error_msg,
                "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth."
            }));
        }

        if let Some((current, max)) = parse_two_u32s(
            &error_msg,
            "Maximum concurrent subagent runs exceeded:",
            "(max:",
        ) {
            return Ok(json!({
                "status": "forbidden",
                "error_type": "ConcurrentLimitExceeded",
                "current_concurrent": current,
                "max_concurrent": max,
                "error": error_msg,
                "note": "Maximum concurrent subagent runs exceeded. Please wait for existing runs to complete."
            }));
        }

        if let Some(secs) = parse_one_u32(&error_msg, "Subagent execution timed out after", "s")
        {
            return Ok(json!({
                "status": "timeout",
                "error_type": "Timeout",
                "timeout_seconds": secs,
                "error": error_msg,
                "note": "Subagent execution timed out."
            }));
        }

        // 3. Fallback to string matching for untyped errors
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

    /// Render a typed `SpawnError` to the canonical JSON envelope.
    fn spawn_error_to_json(spawn_err: &SpawnError) -> anyhow::Result<serde_json::Value> {
        Ok(match spawn_err {
            SpawnError::DepthLimitExceeded { current, max } => json!({
                "status": "forbidden",
                "error_type": "DepthLimitExceeded",
                "current_depth": current,
                "max_depth": max,
                "error": spawn_err.to_string(),
                "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth."
            }),
            SpawnError::ConcurrentLimitExceeded { current, max } => json!({
                "status": "forbidden",
                "error_type": "ConcurrentLimitExceeded",
                "current_concurrent": current,
                "max_concurrent": max,
                "error": spawn_err.to_string(),
                "note": "Maximum concurrent subagent runs exceeded. Please wait for existing runs to complete."
            }),
            SpawnError::Timeout { seconds } => json!({
                "status": "timeout",
                "error_type": "Timeout",
                "timeout_seconds": seconds,
                "error": spawn_err.to_string(),
                "note": "Subagent execution timed out."
            }),
            SpawnError::ExecutionFailed(msg) => json!({
                "status": "error",
                "error_type": "ExecutionFailed",
                "error": msg,
            }),
        })
    }

    /// `action = "compact"` — flag the target session for engine-driven
    /// compaction at its next run and return immediately (no LLM call,
    /// no completion signal). Guard refusals from the runtime surface
    /// through the same [`Self::format_error_response`] envelope resume
    /// refusals use.
    async fn execute_compact(
        &self,
        args: &AgentArgs,
        caller_session_key: Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        let session_key = args
            .session_key
            .clone()
            .expect("compact requires session_key (validated)");
        let caller = caller_session_key.ok_or_else(|| {
            anyhow::anyhow!(
                "Agent tool action \"compact\" needs the caller's session id — run through the \
                 engine's tool context or configure a session provider"
            )
        })?;
        match self.runtime.request_compaction(&session_key, &caller).await {
            Ok(outcome) => Ok(serde_json::to_value(outcome)?),
            Err(e) => Self::format_error_response(&e),
        }
    }
}

/// Parse two `u32`s from a string of the shape
/// `"<prefix>{a}<sep>{b}<suffix>"`. Returns `None` if the prefix or
/// separator can't be located or the captures don't parse as u32.
fn parse_two_u32s(s: &str, prefix: &str, sep: &str) -> Option<(u32, u32)> {
    let after_prefix = s.find(prefix)? + prefix.len();
    let rest = &s[after_prefix..];
    let sep_idx = rest.find(sep)?;
    let a: u32 = rest[..sep_idx].trim().parse().ok()?;
    let after_sep = &rest[sep_idx + sep.len()..];
    // Skip leading whitespace before scanning digits — the second
    // number is preceded by a space in the SpawnError Display format
    // (e.g. `"... (max: {max})"` becomes `"... (max: 3)"` so `after_sep`
    // is `" 3)"`, not `"3)"`).
    let after_ws = after_sep.trim_start();
    // Stop at the first non-digit character for the second number.
    let end = after_ws
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_ws.len());
    let b: u32 = after_ws[..end].trim().parse().ok()?;
    Some((a, b))
}

/// Parse a single `u32` from a string of the shape
/// `"<prefix>{n}<suffix>"`. Returns `None` if the prefix can't be
/// located or the capture doesn't parse as u32.
fn parse_one_u32(s: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let after_prefix = s.find(prefix)? + prefix.len();
    let rest = &s[after_prefix..];
    let suffix_idx = rest.find(suffix)?;
    rest[..suffix_idx].trim().parse().ok()
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
        r#"Run LLM work on sessions: spawn a sub-agent run (action "new", the default), re-attach a run to a previous spawned session (action "resume"), or flag a session for compaction (action "compact").

The framework applies a constant 5-minute timeout to all tool calls. If the subagent takes longer than 5 minutes, the work is automatically detached to a background task and a receipt is returned.

Actions:
- new (default): Spawn a sub-agent run in an isolated or shared session. Requires prompt + subagent_type.
- resume: Re-attach this run to an existing spawned session you own (session_key from the session tool's list) — the subagent continues with its full prior history. Requires session_key + prompt + subagent_type. Mutually exclusive with isolated.
- compact: Flag a session for engine-driven summarization. Requires session_key only (prompt and subagent_type are ignored if supplied). Returns immediately after flagging the session; the engine summarizes at the target's next run. There is no completion signal; the target's next resume will reflect the compacted history.

Parameters:
- action: "new" | "resume" | "compact" (default: "new")
- prompt: Description of the task to execute (required for new and resume)
- subagent_type: Name of the agent config under ~/.peko/agents/<subagent_type>/config.toml (required for new and resume)
- session_key: Target session (required for resume and compact; ignored for new) — a raw session id from the session tool's list, or an absolute path ('/a/b' of slugs, anchored at the root of your session tree; see the session tool list's `path` field)
- description: Optional description for tracking (matches Claude Code's Agent schema)
- name: Optional slug for the spawned session (new only) — the per-parent-unique path segment so you can later address the child as '/.../<name>' (1-64 chars, no '/', no leading/trailing whitespace; must be unique among your session's existing children). If your subtree already contains a STANDING session with this slug (declared via the principal's [children] config), the run attaches to that session with its full history instead of spawning fresh; the subagent_type must match the declaration. A name colliding with a non-standing session is an error — rename semantics live in the session tool.
- model: Optional model override for the subagent (matches Claude Code's Agent schema)
- isolated: If true, creates isolated session without parent context (default: false)
- cleanup: "keep" or "delete" - what to do with session after completion (default: "keep")
- parent_session_key: Parent session key for context seeding (optional - auto-detected if not provided; must be your own session or one inside your subtree)

Sessions you spawn have `parent_session_id` set to your session; the `session` tool's `list` action surfaces this. Use that field to find them. The session tool manages memory; this tool runs work.

resume refusals (structured errors naming the session): target must exist, be a spawned session (branches/live root sessions refuse), not be the session you are running in or an ancestor, not be archived, and not have an active run. compact refusals: target must exist, not be the session you are running in or an ancestor (the engine compacts those automatically), be inside your subtree, not be archived, and not have an active run.

Limits:
- Spawn depth is capped at 3 levels from the root. Attempting a deeper
  chain returns `error_type: "DepthLimitExceeded"` with `current_depth`
  and `max_depth` fields — count the depth of your current subagent
  chain (root + every Agent call in the lineage) before spawning and
  stop at depth 3 to avoid the rejection.
- Up to 5 concurrent subagent runs are allowed; exceeding that returns
  `error_type: "ConcurrentLimitExceeded"` with `current_concurrent` and
  `max_concurrent` fields.

Examples:
// Blocking spawn - parent waits for result (auto-detaches on timeout)
{"prompt": "Use Write to create report.txt with a summary", "subagent_type": "writer"}

// Isolated context - fresh session
{"prompt": "Analyze confidential data", "subagent_type": "analyst", "isolated": true, "cleanup": "delete"}

// Persistent worker - continue a previous spawned session with its history
{"action": "resume", "session_key": "<session-id from session list>", "prompt": "Now update report.txt with the new numbers", "subagent_type": "writer"}

// Compact a long transcript before the next resume
{"action": "compact", "session_key": "<session-id from session list>"}"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["new", "resume", "compact"],
                    "description": "What to do: 'new' spawns a fresh subagent session (default), 'resume' re-attaches a run to an existing spawned session, 'compact' flags a session for engine-driven summarization at its next run",
                    "default": "new"
                },
                "prompt": {
                    "type": "string",
                    "description": "Description of the task to execute (required for new and resume; ignored for compact)"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "Name of the agent config under ~/.peko/agents/<subagent_type>/config.toml (required for new and resume; ignored for compact)"
                },
                "session_key": {
                    "type": "string",
                    "description": "Target session: a raw session id from the session tool's list, or an absolute path ('/a/b' of slugs anchored at the root of your session tree). Required for resume and compact. Ignored for new."
                },
                "description": {
                    "type": "string",
                    "description": "Optional description for tracking this spawn"
                },
                "name": {
                    "type": "string",
                    "description": "Optional slug for the spawned session (new only): the per-parent-unique path segment for later '/.../<name>' addressing (1-64 chars, no '/', no leading/trailing whitespace; must be unique among your session's children). A slug matching an existing STANDING session in your subtree attaches to it instead of spawning fresh."
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
                    "enum": ["keep", "delete"],
                    "description": "What to do with session after completion: 'keep' or 'delete'",
                    "default": "keep"
                },
                "parent_session_key": {
                    "type": "string",
                    "description": "Parent session key for context seeding (auto-detected if not provided; must be your own session or one inside your subtree)"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: AgentArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let action = parse_action(&args.action)?;
        validate_action_args(action, &args)?;
        let cleanup = parse_cleanup(args.cleanup.as_deref())?;
        let resume_session = resume_target(action, &args);

        // The caller's own session (auto-detected) vs an explicit
        // parent_session_key (context seeding) — the adapter
        // ownership-validates the explicit one against the caller's.
        let caller_session_key = self
            .session_provider
            .as_ref()
            .map(|p| p.current_session_key());

        if action == AgentAction::Compact {
            return self.execute_compact(&args, caller_session_key).await;
        }

        let parent_session_key = match args.parent_session_key.clone() {
            Some(key) => key,
            None => match caller_session_key.clone() {
                Some(key) => key,
                None => {
                    return Err(anyhow::anyhow!(
                        "Agent tool requires a parent_session_key parameter or session provider. \
                        Please provide parent_session_key in the tool parameters."
                    ));
                }
            },
        };

        // Resolve subagent_type to a concrete agent config and apply model override.
        let _subagent_config = self
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
            // Phase 1: forward the parent-driven model id onto the
            // execution config; the adapter projects it onto the
            // root-side `ExecutionConfig.model_override` and
            // `execute_subagent_task` clones the inherited provider
            // with this id stamped on `default_model_id`.
            model_override: args.model.clone(),
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
            args.model.clone(),
            resume_session,
            args.name.clone(),
            caller_session_key,
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

        let action = parse_action(&args.action)?;
        validate_action_args(action, &args)?;
        let cleanup = parse_cleanup(args.cleanup.as_deref())?;
        let resume_session = resume_target(action, &args);

        // The caller's own session id comes from the engine's tool
        // context on the production path (see below); the session-key
        // provider is the fallback.
        let caller_session_key = ctx.session_id.clone().or_else(|| {
            self.session_provider
                .as_ref()
                .map(|p| p.current_session_key())
        });

        if action == AgentAction::Compact {
            return self.execute_compact(&args, caller_session_key).await;
        }

        let parent_session_key = if let Some(key) = args.parent_session_key.clone() {
            key
        } else if let Some(ref sid) = ctx.session_id {
            // The engine's tool executor always supplies the real
            // session id here; prefer it over the session-key provider,
            // whose daemon-path placeholder ("agent:<name>:cli:default")
            // names no session and breaks parent linkage in the session
            // index (2026-08-07 field test, Finding 7).
            sid.clone()
        } else if let Some(ref provider) = self.session_provider {
            provider.current_session_key()
        } else {
            return Err(anyhow::anyhow!(
                "Agent tool requires a parent_session_key parameter or session provider."
            ));
        };

        let _subagent_config = self
            .resolve_subagent_config(&args.subagent_type, args.model.as_deref())
            .await?;

        let config = ExecutionConfig {
            timeout_seconds: 300,
            cleanup,
            label: args.description.clone(),
            announce_completion: true,
            max_depth: self.max_depth,
            // Phase 1: see `execute` above — same forwarding
            // pattern.
            model_override: args.model.clone(),
        };
        self.execute_spawn_blocking(
            &args.prompt,
            &args.subagent_type,
            args.isolated,
            &parent_session_key,
            config,
            args.description,
            cleanup,
            args.model.clone(),
            resume_session,
            args.name.clone(),
            caller_session_key,
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
    configs: std::collections::HashMap<String, crate::tools::builtin::messaging::dto::AgentConfig>,
    /// Audit log of spawn events.
    audits: Vec<SpawnAuditEvent>,
    /// Phase 1: every `model_override` seen in
    /// `resolve_agent_config` calls. Tests assert on this to
    /// confirm the parent-driven model id is forwarded all the
    /// way to the resolve path.
    model_overrides_seen: Vec<Option<String>>,
    /// Every `name` (child slug) seen in `execute_and_wait` requests,
    /// in order. Tests assert the Agent tool `name` param is
    /// forwarded onto the spawn request.
    names_seen: Vec<Option<String>>,
    /// Whether `execute_and_wait` should succeed (true) or fail with
    /// an error (false).
    succeed_on_execute: bool,
    /// Every `request_compaction(target, caller)` call seen, in order.
    compaction_requests: Vec<(String, String)>,
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
                model_overrides_seen: Vec::new(),
                names_seen: Vec::new(),
                succeed_on_execute: true,
                compaction_requests: Vec::new(),
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
        config: crate::tools::builtin::messaging::dto::AgentConfig,
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

    /// Phase 1: every `model_override` seen in
    /// `resolve_agent_config` calls (in order). Tests use this to
    /// confirm the parent-driven model id is forwarded from the
    /// JSON schema all the way to the resolve path.
    #[must_use]
    pub fn model_overrides_seen(&self) -> Vec<Option<String>> {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .model_overrides_seen
            .clone()
    }

    /// Every `name` (child slug) seen in `execute_and_wait` requests.
    #[must_use]
    pub fn names_seen(&self) -> Vec<Option<String>> {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .names_seen
            .clone()
    }

    /// Whether the runtime should succeed on `execute_and_wait`.
    pub fn set_succeed_on_execute(&self, succeed: bool) {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .succeed_on_execute = succeed;
    }

    /// Every `request_compaction(target, caller)` call seen (cloned).
    #[must_use]
    pub fn compaction_requests(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .compaction_requests
            .clone()
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
        if state.grants.is_empty() {
            return false;
        }
        let required = format!("agent:{subagent_type}");
        state.grants.iter().any(|g| g == &required)
    }

    async fn resolve_agent_config(
        &self,
        name: &str,
        _workspace: Option<&Path>,
        model_override: Option<&str>,
    ) -> anyhow::Result<crate::tools::builtin::messaging::dto::AgentConfig> {
        let mut state = self
            .inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned");
        // Phase 1: record the model override so tests can assert it
        // was forwarded.
        state
            .model_overrides_seen
            .push(model_override.map(str::to_string));
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
    ) -> anyhow::Result<crate::tools::builtin::messaging::dto::SubagentRunView> {
        let succeed = {
            let mut state = self
                .inner
                .lock()
                .expect("TestSubagentRuntime mutex poisoned");
            state.names_seen.push(request.name.clone());
            state.succeed_on_execute
        };
        if !succeed {
            return Err(anyhow::anyhow!("test failure"));
        }
        Ok(crate::tools::builtin::messaging::dto::SubagentRunView {
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

    async fn request_compaction(
        &self,
        target: &str,
        caller_session_key: &str,
    ) -> anyhow::Result<crate::tools::builtin::session::CompactRequestOutcome> {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .compaction_requests
            .push((target.to_string(), caller_session_key.to_string()));
        Ok(crate::tools::builtin::session::CompactRequestOutcome {
            session_id: target.to_string(),
            message: "Compaction scheduled".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::messaging::dto::AgentConfig;

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
    async fn test_agent_state_registry_unregistered_principal_is_fail_closed() {
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

        // No grants registered: missing authorization context is denied.
        let result = tool.resolve_subagent_config("writer", None).await;
        assert!(result.is_err(), "unregistered principal should fail closed");
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
        assert_eq!(response["error_type"].as_str().unwrap(), "DepthLimitExceeded");
        assert_eq!(response["current_depth"].as_u64().unwrap(), 4);
        assert_eq!(response["max_depth"].as_u64().unwrap(), 3);
        assert!(response["note"].as_str().unwrap().contains("depth"));
        assert!(response["error"].as_str().unwrap().contains('4'));

        // Test typed concurrent error
        let concurrent_err =
            anyhow::anyhow!(SpawnError::ConcurrentLimitExceeded { current: 5, max: 5 });
        let response = AgentTool::format_error_response(&concurrent_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "forbidden");
        assert_eq!(
            response["error_type"].as_str().unwrap(),
            "ConcurrentLimitExceeded"
        );
        assert_eq!(response["current_concurrent"].as_u64().unwrap(), 5);
        assert_eq!(response["max_concurrent"].as_u64().unwrap(), 5);
        assert!(response["note"].as_str().unwrap().contains("concurrent"));

        // Test typed timeout error
        let timeout_err = anyhow::anyhow!(SpawnError::Timeout { seconds: 30 });
        let response = AgentTool::format_error_response(&timeout_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "timeout");
        assert_eq!(response["error_type"].as_str().unwrap(), "Timeout");
        assert_eq!(response["timeout_seconds"].as_u64().unwrap(), 30);

        // Test typed execution failed error
        let exec_err = anyhow::anyhow!(SpawnError::ExecutionFailed(
            "something went wrong".to_string()
        ));
        let response = AgentTool::format_error_response(&exec_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "error");
        assert_eq!(
            response["error_type"].as_str().unwrap(),
            "ExecutionFailed"
        );
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("something went wrong"));

        // Field-test fix (2026-08-02): the async-exec layer stringifies
        // `SpawnError` into `AsyncTaskStatus::Failed { error: String }`
        // before it reaches us, destroying the typed chain. Verify the
        // Display-string fallback reconstructs the typed fields.
        let stringified_depth = anyhow::anyhow!(
            "Subagent failed: Maximum spawn depth exceeded: 4 (max: 3)"
        );
        let response = AgentTool::format_error_response(&stringified_depth).unwrap();
        assert_eq!(response["error_type"].as_str().unwrap(), "DepthLimitExceeded");
        assert_eq!(response["current_depth"].as_u64().unwrap(), 4);
        assert_eq!(response["max_depth"].as_u64().unwrap(), 3);

        let stringified_concurrent = anyhow::anyhow!(
            "Subagent failed: Maximum concurrent subagent runs exceeded: 6 (max: 5)"
        );
        let response = AgentTool::format_error_response(&stringified_concurrent).unwrap();
        assert_eq!(
            response["error_type"].as_str().unwrap(),
            "ConcurrentLimitExceeded"
        );
        assert_eq!(response["current_concurrent"].as_u64().unwrap(), 6);
        assert_eq!(response["max_concurrent"].as_u64().unwrap(), 5);

        let stringified_timeout =
            anyhow::anyhow!("Subagent execution timed out after 300s");
        let response = AgentTool::format_error_response(&stringified_timeout).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "timeout");
        assert_eq!(response["timeout_seconds"].as_u64().unwrap(), 300);

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
        // Phase 1: `model` is absent from the legacy JSON, parses
        // as `None` (`#[serde(skip_serializing_if = "Option::is_none")]`).
        assert_eq!(args.model, None);
    }

    #[test]
    fn test_args_parsing_with_model() {
        // Phase 1: round-trip the parent-driven `model` field
        // through `AgentArgs`. Without this, callers cannot pick
        // a model for a subagent — the field is dropped on
        // deserialization.
        let json = r#"{
            "prompt": "Do something",
            "subagent_type": "writer",
            "model": "claude-haiku-4-5"
        }"#;

        let args: AgentArgs = serde_json::from_str(json).unwrap();
        assert_eq!(
            args.model,
            Some("claude-haiku-4-5".to_string()),
            "model must round-trip through AgentArgs"
        );
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
                    model_override: None,
                },
                Some("my-task".into()),
                SpawnCleanupPolicy::Keep,
                None, // model override
                None, // resume_session
                None, // name (child slug)
                None, // caller_session_key
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
        // Phase 1: no model override in this test → audit row
        // records `model_id: None`.
        assert_eq!(audits[0].model_id, None);
    }

    #[tokio::test]
    async fn test_audit_records_model_override_on_spawn() {
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

        // Phase 1: parent picks a model. The audit row must carry
        // `model_id: Some("haiku-4")` so `peko audit tail` shows the
        // parent-driven model choice. `execute_spawn_blocking`
        // forwards `model` straight into `SpawnAuditEvent.model_id`.
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
                    model_override: Some("haiku-4".to_string()),
                },
                Some("my-task".into()),
                SpawnCleanupPolicy::Keep,
                Some("haiku-4".to_string()),
                None, // resume_session
                None, // name (child slug)
                None, // caller_session_key
                None,
            )
            .await
            .unwrap();

        assert_eq!(result["status"], "completed");

        let audits = runtime.audits();
        assert_eq!(audits.len(), 1);
        assert_eq!(
            audits[0].model_id,
            Some("haiku-4".to_string()),
            "audit row must carry the parent-driven model id"
        );

        // Phase 1: `execute_spawn_blocking` calls `resolve_agent_config`
        // with the model override. (The pre-validate
        // `resolve_subagent_config` in `execute_with_context` is
        // bypassed because this test calls
        // `execute_spawn_blocking` directly.) Pre-Phase-1 the
        // resolve path dropped the override via `_model_override`,
        // so the recorded list would have been `[None]` here.
        let seen = runtime.model_overrides_seen();
        assert_eq!(
            seen,
            vec![Some("haiku-4".to_string())],
            "resolve must see the model override, got: {seen:?}"
        );
    }

    #[tokio::test]
    async fn test_resume_with_isolated_is_contradictory() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.grant("agent:writer");
        runtime.register_agent(
            "writer",
            AgentConfig {
                name: "writer".into(),
                ..Default::default()
            },
        );
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

        // action=resume + isolated is a structured refusal before any
        // session/provider plumbing runs.
        let err = tool
            .execute(serde_json::json!({
                "action": "resume",
                "session_key": "some-session",
                "prompt": "x",
                "subagent_type": "writer",
                "isolated": true,
            }))
            .await
            .expect_err("action=resume + isolated must refuse");
        assert!(err.to_string().contains("contradictory"), "{err}");
    }

    #[tokio::test]
    async fn test_action_schema_surface() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime as SharedSubagentRuntime);
        let params = tool.parameters();
        assert_eq!(
            params["properties"]["action"]["enum"],
            serde_json::json!(["new", "resume", "compact"])
        );
        assert_eq!(params["properties"]["session_key"]["type"], "string");
        assert_eq!(
            params["properties"]["cleanup"]["enum"],
            serde_json::json!(["keep", "delete"])
        );
        // The schema `required` list is empty by design — per-action
        // requirements (new: prompt+subagent_type; resume:
        // session_key+prompt+subagent_type; compact: session_key) are
        // validated in code.
        assert!(params.get("required").is_none());
        assert!(params["properties"].get("resume_session").is_none());
        // Coin-model cross-reference in the description.
        let desc = tool.description();
        assert!(desc.contains("resume"));
        assert!(desc.contains("compact"));
        assert!(desc.contains("session"));
    }

    #[test]
    fn test_action_defaults_to_new_when_omitted() {
        let args: AgentArgs =
            serde_json::from_str(r#"{"prompt": "Do something", "subagent_type": "writer"}"#)
                .unwrap();
        assert_eq!(args.action, "new");
        assert_eq!(args.session_key, None);
    }

    #[tokio::test]
    async fn test_unknown_action_is_structured_error() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);
        let err = tool
            .execute(serde_json::json!({
                "action": "purge",
                "prompt": "x",
                "subagent_type": "writer",
            }))
            .await
            .expect_err("unknown action must refuse");
        let msg = err.to_string();
        assert!(msg.contains("purge"), "{msg}");
        assert!(msg.contains("\"new\", \"resume\", \"compact\""), "{msg}");
    }

    #[tokio::test]
    async fn test_resume_requires_session_key() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);
        let err = tool
            .execute(serde_json::json!({
                "action": "resume",
                "prompt": "x",
                "subagent_type": "writer",
            }))
            .await
            .expect_err("resume without session_key must refuse");
        assert!(err.to_string().contains("session_key"), "{err}");
    }

    #[tokio::test]
    async fn test_new_requires_prompt_and_subagent_type() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);
        let err = tool
            .execute(serde_json::json!({ "subagent_type": "writer" }))
            .await
            .expect_err("new without prompt must refuse");
        assert!(err.to_string().contains("prompt"), "{err}");
    }

    #[tokio::test]
    async fn test_compact_routes_to_request_compaction() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let provider = Box::new(StaticSessionKeyProvider::new("caller:sess"));
        let tool =
            AgentTool::with_session_provider(runtime.clone() as SharedSubagentRuntime, provider);

        let result = tool
            .execute(serde_json::json!({
                "action": "compact",
                "session_key": "target-sess",
            }))
            .await
            .expect("compact should succeed against the test runtime");

        assert_eq!(result["session_id"], "target-sess");
        assert!(result["message"].as_str().unwrap().contains("Compaction"));
        assert_eq!(
            runtime.compaction_requests(),
            vec![("target-sess".to_string(), "caller:sess".to_string())],
            "compact must route to the port with the caller's session id"
        );
        // No spawn ran, so nothing was audited.
        assert!(runtime.audits().is_empty());
    }

    #[tokio::test]
    async fn test_compact_requires_session_key() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let provider = Box::new(StaticSessionKeyProvider::new("caller:sess"));
        let tool =
            AgentTool::with_session_provider(runtime.clone() as SharedSubagentRuntime, provider);
        let err = tool
            .execute(serde_json::json!({ "action": "compact" }))
            .await
            .expect_err("compact without session_key must refuse");
        assert!(err.to_string().contains("session_key"), "{err}");
        assert!(runtime.compaction_requests().is_empty());
    }

    #[tokio::test]
    async fn test_cleanup_unknown_value_errors() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.grant("agent:writer");
        runtime.register_agent(
            "writer",
            AgentConfig {
                name: "writer".into(),
                ..Default::default()
            },
        );
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

        // Round-7 audit fix: an unknown cleanup value used to silently
        // become Keep; it is now a structured validation error naming
        // the bad value and the valid set.
        let err = tool
            .execute(serde_json::json!({
                "prompt": "x",
                "subagent_type": "writer",
                "cleanup": "purge",
            }))
            .await
            .expect_err("unknown cleanup must refuse");
        let msg = err.to_string();
        assert!(msg.contains("purge"), "{msg}");
        assert!(msg.contains("\"keep\", \"delete\""), "{msg}");
    }

    #[test]
    fn test_parse_cleanup_accepts_valid_values_case_insensitively() {
        assert_eq!(parse_cleanup(None).unwrap(), SpawnCleanupPolicy::Keep);
        assert_eq!(
            parse_cleanup(Some("keep")).unwrap(),
            SpawnCleanupPolicy::Keep
        );
        assert_eq!(
            parse_cleanup(Some("DELETE")).unwrap(),
            SpawnCleanupPolicy::Delete
        );
        assert!(parse_cleanup(Some("purge")).is_err());
    }

    #[tokio::test]
    async fn test_new_with_name_forwards_child_slug() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.grant("agent:writer");
        runtime.register_agent(
            "writer",
            AgentConfig {
                name: "writer".into(),
                ..Default::default()
            },
        );
        let provider = Box::new(StaticSessionKeyProvider::new("caller:sess"));
        let tool =
            AgentTool::with_session_provider(runtime.clone() as SharedSubagentRuntime, provider);

        let result = tool
            .execute(serde_json::json!({
                "prompt": "x",
                "subagent_type": "writer",
                "name": "task-b",
            }))
            .await
            .expect("spawn with name should succeed against the test runtime");
        assert_eq!(result["status"], "completed");

        // The `name` param lands on SpawnRequest.name (the adapter
        // projects it onto the child session's slug).
        assert_eq!(runtime.names_seen(), vec![Some("task-b".to_string())]);

        // Schema + description surface the param.
        let params = tool.parameters();
        assert_eq!(params["properties"]["name"]["type"], "string");
        assert!(tool.description().contains("name"));
    }

    #[tokio::test]
    async fn test_name_defaults_to_none() {
        let args: AgentArgs =
            serde_json::from_str(r#"{"prompt": "Do something", "subagent_type": "writer"}"#)
                .unwrap();
        assert_eq!(args.name, None);
    }
}
