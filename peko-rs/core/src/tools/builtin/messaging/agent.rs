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
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use crate::tools::builtin::messaging::subagent_runtime::SubagentRuntime;
use crate::tools::builtin::messaging::subagent_runtime::{
    SharedSubagentRuntime, SpawnAuditEvent, SpawnRequest,
};

/// Trait for providing the current session key
///
/// This allows the tool to get the current session key at execution time,
/// even though the session is determined at runtime.
/// Agent tool arguments.
///
/// Trimmed surface (sprint 7, 2026-08-21): the LLM-facing tool takes
/// only what the spawn actually needs.
///
/// - `action` selects the mode (`new` / `resume`; `compact` stays
///   for now per the round-7 decision).
/// - `path` is the slug path the target session lives at, or will
///   live at on `new`. Required non-empty for `new` (the new
///   session's address) and `resume` (the session to re-attach to).
///   Full (`/local-user/daily-newsfeed`) and caller-relative
///   (`daily-newsfeed`) paths are both accepted; raw UUIDs are
///   refused at the runtime layer via `resolve_reference`.
/// - `prompt` is the task description.
/// - `subagent_type` names the agent config to load.
/// - `model` is an optional override.
///
/// Removed in this commit (vs the previous round-7 surface):
/// `description`, `isolated`, `cleanup`, `parent_session_key`,
/// `session_key`, `name`. `session_key` and `name` are unified into
/// `path`. The rest were either unused at the LLM surface
/// (`description`, `parent_session_key`), ambiguous (`isolated` —
/// "no parent context" has no clean answer for a subagent), or
/// overlapping with the session tool's domain (`cleanup`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentArgs {
    /// Action to perform: "new" (default), "resume", or "compact".
    #[serde(default = "default_action")]
    pub action: String,
    /// Path under which the target session lives (`resume`) or will
    /// live (`new`). Full (`/a/b/c`) or caller-relative (`b/c`); never
    /// a raw UUID. Required for `new` and `resume`; ignored for
    /// `compact`.
    #[serde(default)]
    pub path: String,
    /// Task description / prompt for the subagent (required for `new`
    /// and `resume`; ignored for `compact`).
    #[serde(default)]
    pub prompt: String,
    /// Subagent type: name of the agent config under ~/.peko/agents/<subagent_type>/config.toml
    /// (required for `new` and `resume`; ignored for `compact`).
    #[serde(default)]
    pub subagent_type: String,
    /// Optional model override for the subagent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
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
            // The new session's address — and the address of every
            // child of it — is its path; raw ids are refused at the
            // runtime layer via `resolve_reference`. Validate the
            // segment shape here so the model gets an actionable
            // error before the runtime touches state.
            if args.path.is_empty() {
                return Err(anyhow::anyhow!(
                    "action \"new\" requires 'path' — a full ('/a/b/c') or caller-relative \
                     ('agent-c') slug path under which the new session will live (no raw UUIDs)"
                ));
            }
            if let Err(e) = peko_session::path::validate_path(&args.path) {
                return Err(anyhow::anyhow!(
                    "action \"new\" 'path' is not a valid slug path: {e}"
                ));
            }
        }
        AgentAction::Resume => {
            if args.path.is_empty() {
                return Err(anyhow::anyhow!(
                    "action \"resume\" requires 'path' — a full ('/a/b/c') or caller-relative \
                     ('agent-c') slug path naming the session to re-attach to (no raw UUIDs)"
                ));
            }
            if let Err(e) = peko_session::path::validate_path(&args.path) {
                return Err(anyhow::anyhow!(
                    "action \"resume\" 'path' is not a valid slug path: {e}"
                ));
            }
            if args.prompt.is_empty() || args.subagent_type.is_empty() {
                return Err(anyhow::anyhow!(
                    "action \"resume\" requires 'prompt' and 'subagent_type'"
                ));
            }
        }
        AgentAction::Compact => {
            if args.path.is_empty() {
                return Err(anyhow::anyhow!(
                    "action \"compact\" requires 'path' — a slug path naming the session to \
                     flag for compaction"
                ));
            }
        }
    }
    Ok(())
}

/// Agent tool
///
/// Creates a subagent session and executes a task in the background.
/// Results are announced back to the parent when complete.
pub struct AgentTool {
    /// Runtime port — the only seam between the tool and the
    /// daemon/agent state. Sprint 7 collapsed the previous
    /// `workspace` / `session_provider` / `max_depth` /
    /// `max_concurrent` fields onto the port: workspace is the
    /// runtime's bound principal workspace, the caller session
    /// id comes from `ToolContext::session_id` (with the runtime
    /// port's `session_id()` accessor as the fallback), and the
    /// depth / concurrency caps live in the executor itself
    /// (`SubagentExecutor::max_concurrent`). The tool reads the
    /// spawn-depth cap via [`SubagentRuntime::max_depth`] and
    /// forwards it onto `ExecutionConfig.max_depth` so the
    /// executor's per-spawn gate sees it.
    runtime: SharedSubagentRuntime,
}

impl AgentTool {
    /// Create a new Agent tool with a runtime port.
    #[must_use]
    pub fn new(runtime: SharedSubagentRuntime) -> Self {
        Self { runtime }
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
            .resolve_agent_config(subagent_type, self.runtime.workspace(), model_override)
            .await
    }

    /// Execute the spawn path (new + resume).
    ///
    /// `path` is the LLM-facing slug path: for `new` it's the new
    /// session's slug (projected onto `SpawnRequest.name`); for
    /// `resume` it's the target session's path (projected onto
    /// `SpawnRequest.resume_session`). The runtime port resolves the
    /// LLM-facing path to a canonical UUID internally.
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
        path: &str,
        model: Option<String>,
        resume_session: Option<String>,
        caller_session_key: Option<String>,
        ctx: Option<&ToolContext>,
    ) -> anyhow::Result<serde_json::Value> {
        let timeout_seconds: u64 = 300;

        // Resolve the subagent config first so we can audit with
        // the resolved name (and so `audit_spawn` runs even when the
        // spawn is later blocked by a runtime error).
        let subagent_config = self
            .runtime
            .resolve_agent_config(subagent_type, self.runtime.workspace(), model.as_deref())
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
                parent_session_key: caller_session_key.clone().unwrap_or_default(),
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
                parent_session_key: caller_session_key.clone().unwrap_or_default(),
                config: crate::tools::builtin::messaging::dto::ExecutionConfig {
                    timeout_seconds,
                    announce_completion: true,
                    max_depth: self.runtime.max_depth(),
                    model_override: model.clone(),
                },
                timeout_seconds,
                parent_cancel,
                subagent_config,
                model: model.clone(),
                resume_session,
                caller_session_key,
                name: Some(path.to_string()),
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
                    "timeout_seconds": timeout_seconds,
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
    /// Classifies the error using a typed `SpawnError` when available,
    /// falling back to string matching only for untyped errors. Walks
    /// the `anyhow` chain first; if no typed error is found (the async
    /// exec layer stringifies the error at `executor.rs:343` before it
    /// reaches us), parses the well-defined `SpawnError` Display
    /// format to reconstruct the typed fields.
    fn format_error_response(
        error: &anyhow::Error,
    ) -> anyhow::Result<serde_json::Value> {
        // 1. Try typed classification first, walking the anyhow chain
        //    because intermediate layers re-wrap the typed error with
        //    a string-formatted `anyhow!`.
        for source in error.chain() {
            if let Some(spawn_err) = source.downcast_ref::<crate::tools::builtin::messaging::dto::SpawnError>() {
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
        //        → "Subagent execution timed out after {seconds} seconds"
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

        if let Some(secs) = parse_one_u32(&error_msg, "Subagent execution timed out after", "seconds")
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
    fn spawn_error_to_json(
        spawn_err: &crate::tools::builtin::messaging::dto::SpawnError,
    ) -> anyhow::Result<serde_json::Value> {
        Ok(match spawn_err {
            crate::tools::builtin::messaging::dto::SpawnError::DepthLimitExceeded { current, max } => json!({
                "status": "forbidden",
                "error_type": "DepthLimitExceeded",
                "current_depth": current,
                "max_depth": max,
                "error": spawn_err.to_string(),
                "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth."
            }),
            crate::tools::builtin::messaging::dto::SpawnError::ConcurrentLimitExceeded { current, max } => json!({
                "status": "forbidden",
                "error_type": "ConcurrentLimitExceeded",
                "current_concurrent": current,
                "max_concurrent": max,
                "error": spawn_err.to_string(),
                "note": "Maximum concurrent subagent runs exceeded. Please wait for existing runs to complete."
            }),
            crate::tools::builtin::messaging::dto::SpawnError::Timeout { seconds } => json!({
                "status": "timeout",
                "error_type": "Timeout",
                "timeout_seconds": seconds,
                "error": spawn_err.to_string(),
                "note": "Subagent execution timed out."
            }),
            crate::tools::builtin::messaging::dto::SpawnError::ExecutionFailed(msg) => json!({
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
        let session_key = args.path.clone();
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
- new (default): Spawn a sub-agent run in a new session under your tree. Requires prompt + subagent_type + path. The `path` is the slug path under which the new session will live — full (`/local-user/daily-newsfeed`) or caller-relative (`daily-newsfeed`). The last segment is the new session's slug; intermediate segments describe where in the tree it lives. Raw UUIDs are REFUSED.
- resume: Re-attach this run to an existing spawned session you own. `path` follows the same slug-path / caller-relative grammar as `session list`'s `path` field. Requires path + prompt + subagent_type.
- compact: Flag a session for engine-driven summarization. `path` follows the same slug-path / caller-relative grammar. Requires path only (prompt and subagent_type are ignored if supplied). Returns immediately after flagging; the engine summarizes at the target's next run.

Parameters:
- action: "new" | "resume" | "compact" (default: "new")
- prompt: Description of the task to execute (required for new and resume)
- subagent_type: Name of the agent config under ~/.peko/agents/<subagent_type>/config.toml (required for new and resume)
- path: Target session (required for resume and compact) — a slug path ('/a/b/c') or a caller-relative slug ('agent-c') from the session tool's list (`path` field). Raw session ids are refused.
  For new: the slug path under which the new session will live; the last segment is the new session's slug.
- model: Optional model override for the subagent (matches Claude Code's Agent schema)

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
{"prompt": "Use Write to create report.txt with a summary", "subagent_type": "writer", "path": "writer-1"}

// Persistent worker - continue a previous spawned session with its history
{"action": "resume", "path": "/writer-1", "prompt": "Now update report.txt with the new numbers", "subagent_type": "writer"}

// Compact a long transcript before the next resume
{"action": "compact", "path": "/writer-1"}"#
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
                "path": {
                    "type": "string",
                    "description": "Target session: an absolute slug path ('/a/b/c' from your tree root) or a caller-relative slug ('agent-c' matching one of your descendants). Raw session ids are refused. Required for resume and compact. For new: the slug path under which the new session will live (last segment is the new session's slug)."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for the subagent"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: AgentArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let action = parse_action(&args.action)?;
        validate_action_args(action, &args)?;

        // The caller's own session — read from the runtime port as the
        // fallback for non-`ToolContext` callers. The production
        // `execute_with_context` path prefers `ctx.session_id`.
        let caller_session_key = self.runtime.session_id();

        if action == AgentAction::Compact {
            return self.execute_compact(&args, caller_session_key).await;
        }

        // Resolve subagent_type to a concrete agent config and apply
        // model override. This pre-validates the spawn's agent-side
        // shape (capability grant, on-disk config presence) so the
        // runtime can't be reached with a bad config.
        let _subagent_config = self
            .resolve_subagent_config(&args.subagent_type, args.model.as_deref())
            .await?;

        // For `new`, `path` is the new session's slug path (the last
        // segment becomes the child slug). For `resume`, `path`
        // identifies the existing target session; the runtime port
        // resolves it to a canonical UUID and feeds it as
        // `SpawnRequest.resume_session`.
        let resume_session = match action {
            AgentAction::Resume => Some(args.path.clone()),
            AgentAction::New | AgentAction::Compact => None,
        };

        self.execute_spawn_blocking(
            &args.prompt,
            &args.subagent_type,
            &args.path,
            args.model.clone(),
            resume_session,
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

        // The caller's own session id comes from the engine's tool
        // context on the production path; the runtime port's
        // `session_id()` is the fallback for non-context paths
        // (tests, async_executor).
        let caller_session_key = ctx
            .session_id
            .clone()
            .or_else(|| self.runtime.session_id());

        if action == AgentAction::Compact {
            return self.execute_compact(&args, caller_session_key).await;
        }

        let _subagent_config = self
            .resolve_subagent_config(&args.subagent_type, args.model.as_deref())
            .await?;

        let resume_session = match action {
            AgentAction::Resume => Some(args.path.clone()),
            AgentAction::New | AgentAction::Compact => None,
        };

        self.execute_spawn_blocking(
            &args.prompt,
            &args.subagent_type,
            &args.path,
            args.model.clone(),
            resume_session,
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
    /// in order. Tests assert the Agent tool `path` param is
    /// forwarded onto the spawn request.
    names_seen: Vec<Option<String>>,
    /// Every `resume_session` (path) seen in `execute_and_wait`
    /// requests, in order. Tests assert the resume action's
    /// `path` is forwarded onto the spawn request.
    resume_sessions_seen: Vec<Option<String>>,
    /// Whether `execute_and_wait` should succeed (true) or fail with
    /// an error (false).
    succeed_on_execute: bool,
    /// Every `request_compaction(target, caller)` call seen, in order.
    compaction_requests: Vec<(String, String)>,
    /// Principal id used in audit events.
    principal_id: String,
    /// Principal display name used in audit events.
    principal_name: Option<String>,
    /// Caller session id returned by [`SubagentRuntime::session_id`].
    /// Tests set this to inject a synthetic caller session id on
    /// the no-`ToolContext` path. Default `None` matches the trait
    /// default.
    session_id: Option<String>,
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
                resume_sessions_seen: Vec::new(),
                succeed_on_execute: true,
                compaction_requests: Vec::new(),
                principal_id: String::new(),
                principal_name: None,
                session_id: None,
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

    /// Every `resume_session` (target path) seen in `execute_and_wait`
    /// requests.
    #[must_use]
    pub fn resume_sessions_seen(&self) -> Vec<Option<String>> {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .resume_sessions_seen
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

    /// Set the caller session id returned by
    /// [`SubagentRuntime::session_id`]. Used by tests to inject a
    /// synthetic caller session id on the no-`ToolContext` path so
    /// `AgentTool::execute` (no context) can be exercised without
    /// the runtime-mutable `DynamicSessionKeyProvider` machinery
    /// the production agent_runner used to thread through.
    pub fn set_session_id(&self, id: impl Into<String>) {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .session_id = Some(id.into());
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

    fn session_id(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("TestSubagentRuntime mutex poisoned")
            .session_id
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
            state.resume_sessions_seen.push(request.resume_session.clone());
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
            cleanup: crate::tools::builtin::messaging::dto::SpawnCleanupPolicy::Keep,
            label: None,
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
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

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
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

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
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

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
    async fn test_agent_tool_uses_runtime_session_id() {
        // Sprint 7: the `AgentTool` reads the caller session id from
        // the runtime port's `session_id()` accessor (production
        // path uses `ToolContext::session_id`; this verifies the
        // runtime port fallback path). The port itself is the SoT
        // for the no-context fallback.
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.set_session_id("caller:sess:runtime-port");
        let _tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

        assert_eq!(runtime.session_id(), Some("caller:sess:runtime-port".into()));
    }

    #[test]
    fn test_default_max_depth() {
        // Sprint 7: depth is no longer a tool-level constant — it lives
        // on the runtime port. The runtime port's default is `3`.
        let runtime = TestSubagentRuntime::new();
        assert_eq!(runtime.max_depth(), 3);
    }

    #[test]
    fn test_default_max_concurrent() {
        // Sprint 7: `max_concurrent` moved off the tool onto the
        // `SubagentExecutor` itself (principal-level knob —
        // `SubagentExecutor::new` takes the cap as a constructor
        // argument). The tool no longer carries the constant; this
        // test is a placeholder documenting the move.
    }

    #[tokio::test]
    async fn test_error_response_formatting() {
        // Test typed depth error
        let depth_err = anyhow::anyhow!(crate::tools::builtin::messaging::dto::SpawnError::DepthLimitExceeded {
            current: 4,
            max: 3
        });
        let response = AgentTool::format_error_response(&depth_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "forbidden");
        assert_eq!(response["error_type"].as_str().unwrap(), "DepthLimitExceeded");
        assert_eq!(response["current_depth"].as_u64().unwrap(), 4);
        assert_eq!(response["max_depth"].as_u64().unwrap(), 3);
        assert!(response["note"].as_str().unwrap().contains("depth"));
        assert!(response["error"].as_str().unwrap().contains('4'));

        // Test typed concurrent error
        let concurrent_err = anyhow::anyhow!(
            crate::tools::builtin::messaging::dto::SpawnError::ConcurrentLimitExceeded {
                current: 5,
                max: 5
            }
        );
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
        let timeout_err = anyhow::anyhow!(crate::tools::builtin::messaging::dto::SpawnError::Timeout {
            seconds: 30
        });
        let response = AgentTool::format_error_response(&timeout_err).unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "timeout");
        assert_eq!(response["error_type"].as_str().unwrap(), "Timeout");
        assert_eq!(response["timeout_seconds"].as_u64().unwrap(), 30);

        // Test typed execution failed error
        let exec_err = anyhow::anyhow!(
            crate::tools::builtin::messaging::dto::SpawnError::ExecutionFailed(
                "something went wrong".to_string()
            )
        );
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
            anyhow::anyhow!("Subagent execution timed out after 300 seconds");
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
    fn test_args_parsing_trimmed_surface() {
        // Sprint 7 surface: only action, path, prompt, subagent_type,
        // model are accepted. Removed fields (description, isolated,
        // cleanup, parent_session_key, name, session_key) parse as
        // absent — serde-defaults take over (model is the only
        // optional-with-default field, so it appears as None).
        let json = r#"{
            "prompt": "Do something",
            "subagent_type": "writer",
            "path": "task-b"
        }"#;

        let args: AgentArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.action, "new");
        assert_eq!(args.prompt, "Do something");
        assert_eq!(args.subagent_type, "writer");
        assert_eq!(args.path, "task-b");
        assert_eq!(args.model, None);
    }

    #[test]
    fn test_args_parsing_with_model() {
        // The parent-driven `model` field round-trips through AgentArgs.
        let json = r#"{
            "prompt": "Do something",
            "subagent_type": "writer",
            "path": "task-b",
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
    async fn test_action_schema_surface() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime as SharedSubagentRuntime);
        let params = tool.parameters();
        assert_eq!(
            params["properties"]["action"]["enum"],
            serde_json::json!(["new", "resume", "compact"])
        );
        // Sprint 7: path replaces session_key+name. session_key,
        // name, isolated, cleanup, description, parent_session_key
        // are all GONE from the schema.
        let props = &params["properties"];
        assert!(props.get("path").is_some(), "path property must exist");
        assert_eq!(props["path"]["type"], "string");
        for removed in [
            "session_key",
            "name",
            "isolated",
            "cleanup",
            "description",
            "parent_session_key",
        ] {
            assert!(
                props.get(removed).is_none(),
                "{removed} property must be removed"
            );
        }
        // The schema `required` list is empty by design — per-action
        // requirements (new: path+prompt+subagent_type; resume:
        // path+prompt+subagent_type; compact: path) are validated in
        // code.
        assert!(params.get("required").is_none());
        // Coin-model cross-reference in the description.
        let desc = tool.description();
        assert!(desc.contains("resume"));
        assert!(desc.contains("compact"));
        assert!(desc.contains("path"));
        // Removed fields no longer appear in the description.
        assert!(!desc.contains("session_key"), "description must not mention session_key");
        assert!(!desc.contains("isolated"), "description must not mention isolated");
        assert!(!desc.contains("cleanup"), "description must not mention cleanup");
    }

    #[test]
    fn test_action_defaults_to_new_when_omitted() {
        let args: AgentArgs =
            serde_json::from_str(r#"{"prompt": "x", "subagent_type": "writer", "path": "task-b"}"#)
                .unwrap();
        assert_eq!(args.action, "new");
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
                "path": "task-b",
            }))
            .await
            .expect_err("unknown action must refuse");
        let msg = err.to_string();
        assert!(msg.contains("purge"), "{msg}");
        assert!(msg.contains("\"new\", \"resume\", \"compact\""), "{msg}");
    }

    #[tokio::test]
    async fn test_new_requires_path() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);
        let err = tool
            .execute(serde_json::json!({
                "action": "new",
                "prompt": "x",
                "subagent_type": "writer",
            }))
            .await
            .expect_err("new without path must refuse");
        assert!(err.to_string().contains("path"), "{err}");
    }

    #[tokio::test]
    async fn test_resume_requires_path() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);
        let err = tool
            .execute(serde_json::json!({
                "action": "resume",
                "prompt": "x",
                "subagent_type": "writer",
            }))
            .await
            .expect_err("resume without path must refuse");
        assert!(err.to_string().contains("path"), "{err}");
    }

    #[tokio::test]
    async fn test_new_requires_prompt_and_subagent_type() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);
        let err = tool
            .execute(serde_json::json!({ "path": "task-b", "subagent_type": "writer" }))
            .await
            .expect_err("new without prompt must refuse");
        assert!(err.to_string().contains("prompt"), "{err}");
    }

    #[tokio::test]
    async fn test_new_rejects_invalid_path_shape() {
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
        // `:` is reserved for raw session ids — slugs refuse it
        // outright, so the validation error fires before the
        // runtime touches state.
        let err = tool
            .execute(serde_json::json!({
                "prompt": "x",
                "subagent_type": "writer",
                "path": "agent:writer:peer:user:alice",
            }))
            .await
            .expect_err("':' in path must refuse");
        let msg = err.to_string();
        assert!(msg.contains("valid slug path"), "{msg}");
    }

    #[tokio::test]
    async fn test_compact_routes_to_request_compaction() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.set_session_id("caller:sess");
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

        let result = tool
            .execute(serde_json::json!({
                "action": "compact",
                "path": "target-sess",
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
    async fn test_compact_requires_path() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.set_session_id("caller:sess");
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);
        let err = tool
            .execute(serde_json::json!({ "action": "compact" }))
            .await
            .expect_err("compact without path must refuse");
        assert!(err.to_string().contains("path"), "{err}");
        assert!(runtime.compaction_requests().is_empty());
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

        let result = tool
            .execute_spawn_blocking(
                "do work",
                "writer",
                "task-b",
                None, // model
                None, // resume_session
                Some("caller:sess".to_string()),
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
        assert_eq!(audits[0].parent_session_key, "caller:sess");
        // No model override in this test → audit row records
        // `model_id: None`.
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

        // Parent picks a model. The audit row must carry
        // `model_id: Some("haiku-4")` so `peko audit tail` shows the
        // parent-driven model choice. `execute_spawn_blocking`
        // forwards `model` straight into `SpawnAuditEvent.model_id`.
        let result = tool
            .execute_spawn_blocking(
                "do work",
                "writer",
                "task-b",
                Some("haiku-4".to_string()),
                None,
                Some("caller:sess".to_string()),
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

        // `execute_spawn_blocking` calls `resolve_agent_config`
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
    async fn test_new_with_path_forwards_child_slug() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.grant("agent:writer");
        runtime.register_agent(
            "writer",
            AgentConfig {
                name: "writer".into(),
                ..Default::default()
            },
        );
        runtime.set_session_id("caller:sess");
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

        let result = tool
            .execute(serde_json::json!({
                "prompt": "x",
                "subagent_type": "writer",
                "path": "task-b",
            }))
            .await
            .expect("spawn with path should succeed against the test runtime");
        assert_eq!(result["status"], "completed");

        // The `path` param lands on SpawnRequest.name (the adapter
        // projects it onto the child session's slug).
        assert_eq!(runtime.names_seen(), vec![Some("task-b".to_string())]);
        // For new, resume_session is None.
        assert_eq!(runtime.resume_sessions_seen(), vec![None]);

        // Schema + description surface the param.
        let params = tool.parameters();
        assert_eq!(params["properties"]["path"]["type"], "string");
        assert!(tool.description().contains("path"));
    }

    #[tokio::test]
    async fn test_resume_with_path_forwards_target() {
        let runtime = Arc::new(TestSubagentRuntime::new());
        runtime.grant("agent:writer");
        runtime.register_agent(
            "writer",
            AgentConfig {
                name: "writer".into(),
                ..Default::default()
            },
        );
        runtime.set_session_id("caller:sess");
        let tool = AgentTool::new(runtime.clone() as SharedSubagentRuntime);

        let result = tool
            .execute(serde_json::json!({
                "action": "resume",
                "path": "/writer-1",
                "prompt": "x",
                "subagent_type": "writer",
            }))
            .await
            .expect("resume with path should succeed against the test runtime");
        assert_eq!(result["status"], "completed");

        // The `path` param lands on SpawnRequest.resume_session (not
        // name) for the resume action.
        assert_eq!(runtime.names_seen(), vec![Some("/writer-1".to_string())]);
        assert_eq!(
            runtime.resume_sessions_seen(),
            vec![Some("/writer-1".to_string())]
        );
    }
}