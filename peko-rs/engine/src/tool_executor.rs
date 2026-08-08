//! Tool executor for the agentic loop
//!
//! Encapsulates tool execution via [`ToolFunnel`], including:
//! - Workspace and agent context resolution
//! - Tool execution via `peko_engine::funnel::execute_tool_via_core_with_context`
//! - Session recording of tool results (via [`SessionView`])
//! - Event emission (ToolEnd)
//! - Duration tracking
//!
//! Phase 9b.N.3 lift: previously this module lived at
//! `src/engine/tool_executor.rs` and depended on root-only types
//! (`ExtensionCore`, `Arc<RwLock<Session>>`). The lift introduces two
//! trait ports so the executor no longer needs those root-only types:
//!
//! - [`ToolFunnel`] (peko-extension-host) — abstracts the engine-facing
//!   surface of `ExtensionCore` (gate probe, PreToolUse/PostToolUse
//!   hook firing, F37 funnel).
//! - [`SessionView`] (this crate) — abstracts the single write path
//!   the executor needs (`add_tool_result`) against the session.
//!
//! Root's [`crate::session::Session`] impl lives in
//! `src/engine/session_view_compat.rs` (orphan rule).

use crate::events::AgenticEvent;
use crate::parallel_gate::ParallelGate;
use crate::SessionView;
use anyhow::Result;
use peko_extension_api::ToolFunnel;
use peko_message::ContentBlock;
use peko_message::LlmMessage;
use tracing::{info, warn};

/// Result of executing a single tool call.
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    /// The tool result message to append to the conversation.
    pub message: LlmMessage,
    /// Whether execution succeeded.
    pub success: bool,
}

/// Executes tool calls within the agentic loop.
///
/// Owns the [`ParallelGate`] that serializes non-parallelizable tool
/// dispatches against every other running tool (F33, audit section 3
/// row 3). The loop instantiates one executor per agent and shares it
/// across the try_join_all fan-out so all parallel calls share the
/// same gate.
#[derive(Clone)]
pub struct ToolExecutor {
    parallel_gate: ParallelGate,
    /// P2: per-run identical-failure circuit breaker state. The loop
    /// creates one executor per run, so counts are turn-scoped.
    breaker: std::sync::Arc<IdenticalFailureBreaker>,
}

/// P2 (2026-08-07 field test): identical-failure circuit breaker.
///
/// Tracks consecutive failures per (tool, canonical args) within a
/// single run. After [`MAX_IDENTICAL_FAILURES`] identical failures the
/// executor refuses to dispatch the same call again — retrying an
/// unchanged call never produces a different result, and each retry
/// costs a full history replay (a failing `CronCreate` loop burned
/// 68k input tokens in one turn during the field test).
const MAX_IDENTICAL_FAILURES: u8 = 2;

#[derive(Debug, Default)]
pub(crate) struct IdenticalFailureBreaker {
    counts: std::sync::Mutex<std::collections::HashMap<String, u8>>,
}

impl IdenticalFailureBreaker {
    fn key(name: &str, arguments: &serde_json::Value) -> String {
        format!("{name}:{arguments}")
    }

    /// Returns the refusal message when this call has already failed
    /// [`MAX_IDENTICAL_FAILURES`] times with identical arguments.
    fn refusal(&self, name: &str, arguments: &serde_json::Value) -> Option<String> {
        let counts = self.counts.lock().expect("breaker mutex poisoned");
        let failed = counts
            .get(&Self::key(name, arguments))
            .copied()
            .unwrap_or(0);
        (failed >= MAX_IDENTICAL_FAILURES).then(|| {
            format!(
                "Error: this exact call ({name}) has already failed {failed} times this turn \
                 with identical arguments. Retrying it unchanged will not produce a different \
                 result. Stop retrying, explain the failure to the user, and suggest an \
                 alternative."
            )
        })
    }

    /// Record the outcome of a dispatched call. A success clears the
    /// counter for that (tool, args) pair.
    fn record(&self, name: &str, arguments: &serde_json::Value, success: bool) {
        let mut counts = self.counts.lock().expect("breaker mutex poisoned");
        let key = Self::key(name, arguments);
        if success {
            counts.remove(&key);
        } else {
            *counts.entry(key).or_insert(0) += 1;
        }
    }
}

impl Default for ToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolExecutor {
    /// Create a new `ToolExecutor` with a fresh [`ParallelGate`].
    /// Use this in tests and for one-shot invocations.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parallel_gate: ParallelGate::new(),
            breaker: std::sync::Arc::new(IdenticalFailureBreaker::default()),
        }
    }

    /// Create a new `ToolExecutor` that shares the supplied
    /// [`ParallelGate`]. Used by `AgenticLoop` to ensure every fan-out
    /// in the same loop iteration shares the same gate.
    #[must_use]
    pub fn with_gate(parallel_gate: ParallelGate) -> Self {
        Self {
            parallel_gate,
            breaker: std::sync::Arc::new(IdenticalFailureBreaker::default()),
        }
    }

    /// Borrow the underlying gate (used by tests; production code
    /// shouldn't need this).
    #[must_use]
    pub fn parallel_gate(&self) -> &ParallelGate {
        &self.parallel_gate
    }
    /// Execute a single tool call.
    ///
    /// # Arguments
    /// * `tool_call` - The `ContentBlock::ToolCall` to execute
    /// * `tool_funnel` - The host funnel for tool dispatch (F37 +
    ///   F31x hook firing + F33 gate probe). Implemented on root's
    ///   `ExtensionCore` via `src/engine/extension_core_funnel_compat.rs`.
    /// * `agent_name` - The agent's name (for workspace resolution)
    /// * `agent_workspace` - The agent's configured workspace
    /// * `session` - Session sink for recording tool results
    ///   (via [`SessionView`]).
    /// * `session_id` - The session id, supplied by the caller (the
    ///   agentic loop already holds it for `HookInput::ToolCall`
    ///   stamping). Pre-Phase 9b.N.3 the executor fetched this
    ///   internally via `session.read().await`; the trait port keeps
    ///   the read out of `peko-engine`.
    /// * `run_id` - The current run ID (for events)
    /// * `caller_id` - The resolved caller identity (pekohub sub, API key id,
    ///   or `None` for local CLI invocations) — propagated to the tool via
    ///   `HookInput::ToolCall::caller_id` for per-user permission checks and
    ///   audit logging (issue #17).
    /// * `principal_id` - Principal scope for extension-scoped tool state.
    ///   Required in the agentic-loop path; converted back to an option only
    ///   at the `HookInput::ToolCall` boundary for legacy/standalone callers.
    /// * `principal_name` - Human-readable Principal name for Principal-scoped
    ///   tools (e.g. cron).
    /// * `capabilities` - Per-call capability set used by the execution gate.
    /// * `active_extensions` - Active extension IDs for the current Principal;
    ///   the gate verifies the tool's owning extension is active.
    /// * `cancel` - Soft-interrupt `CancellationToken` (PR #128). Bridged
    ///   inside `execute_tool_via_core_with_context` into the tool
    ///   layer's `AbortSignal` so the trait-default `is_aborted()`
    ///   check works in production.
    /// * `on_event` - Event callback
    ///
    /// Returns the tool result message and success flag.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        tool_call: &ContentBlock,
        tool_funnel: &dyn ToolFunnel,
        agent_name: &str,
        agent_workspace: Option<&std::path::PathBuf>,
        session: &dyn SessionView,
        session_id: &str,
        run_id: &str,
        caller_id: Option<&str>,
        principal_id: &str,
        principal_name: &str,
        capabilities: Option<Vec<String>>,
        active_extensions: Option<Vec<String>>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        on_event: &(dyn Fn(AgenticEvent) + Send + Sync),
    ) -> Result<ToolExecutionResult> {
        let (id, name, arguments) = match tool_call {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => (id, name, arguments),
            _ => {
                return Err(anyhow::anyhow!(
                    "ToolExecutor::execute called with non-ToolCall content block"
                ));
            }
        };

        info!("Executing tool: {} (id: {})", name, id);

        let start_time = std::time::Instant::now();

        // P2: refuse the 3rd+ identical failing call BEFORE any
        // dispatch — no hooks, no gate admission, no tool run.
        if let Some(refusal) = self.breaker.refusal(name, arguments) {
            warn!(
                "Tool '{}' short-circuited by the identical-failure breaker",
                name
            );
            session.add_tool_result(id, name, &refusal, true).await?;
            on_event(AgenticEvent::ToolEnd {
                run_id: run_id.to_string(),
                tool_id: id.clone(),
                result: serde_json::Value::String(refusal.clone()),
                success: false,
                duration_ms: 0,
            });
            let message = LlmMessage::tool_result(id.clone(), name.clone(), refusal, true)
                .with_tool_call_id(id.clone());
            return Ok(ToolExecutionResult {
                message,
                success: false,
            });
        }

        // F33: parallel-execution gate admission. Parallelizable tools
        // take a read-lock (concurrent dispatch OK); non-parallelizable
        // tools (Write, Edit, Bash, cron Create/Delete, task Create/Update)
        // take a write-lock and serialize against every other running
        // tool. The guard is held for the entire dispatch lifecycle —
        // PreToolUse, ToolExecute, PostToolUse, session record — so the
        // tool's atomicity window matches its `parallelizable()` claim.
        //
        // F37 / Phase 9b.N.3: route through `ToolFunnel::is_parallelizable`
        // instead of reaching into the side-table. The trait method
        // delegates to `ExtensionCore::is_parallelizable` in root; the
        // side-table handle stays `pub(crate)`. Returns `true` if the
        // tool isn't registered — the dispatch will fail anyway, and
        // admitting without serializing is the right "no-op" fallback.
        let parallel = tool_funnel.is_parallelizable(name).await;
        let _gate_guard = self.parallel_gate.admit(parallel).await;

        let workspace = agent_workspace.map(|p| p.to_string_lossy().to_string());
        let agent_id = agent_name.to_string();
        let session_id_owned = session_id.to_string();

        // F31x: PreToolUse observe-only hook. The trait method hides
        // `HookPoint` / `HookInput` construction + the 2s `HOOK_TIMEOUT`
        // soft-fail inside the impl on `ExtensionCore` (see
        // `src/engine/extension_core_funnel_compat.rs`). Handlers see
        // the same ToolCall payload the dispatcher will use, but their
        // return value is intentionally discarded (observe-only in v1).
        tool_funnel
            .pre_tool_use(
                name,
                arguments.clone(),
                workspace.clone(),
                Some(agent_id.clone()),
                Some(session_id_owned.clone()),
                caller_id.map(str::to_string),
                Some(principal_id.to_string()),
                Some(principal_name.to_string()),
                capabilities.clone(),
                active_extensions.clone(),
            )
            .await;

        let (tool_result_str, tool_result_json, success) =
            match crate::funnel::execute_tool_via_core_with_context(
                tool_funnel,
                name,
                arguments.clone(),
                workspace.clone(),
                Some(agent_id.clone()),
                Some(session_id_owned.clone()),
                caller_id.map(str::to_string),
                Some(principal_id.to_string()),
                Some(principal_name.to_string()),
                capabilities.clone(),
                active_extensions.clone(),
                cancel,
            )
            .await
            {
                Ok((s, v, ok)) => {
                    if ok {
                        info!("Tool '{}' executed successfully via ExtensionCore", name);
                    }
                    (s, v, ok)
                }
                Err(e) => {
                    warn!("Tool '{}' failed via ExtensionCore: {}", name, e);
                    let s = format!("Error: {e}");
                    (s.clone(), serde_json::Value::String(s), false)
                }
            };

        // P2: record the outcome for the identical-failure breaker.
        self.breaker.record(name, arguments, success);
        // F31x: PostToolUse observe-only hook. Symmetric with PreToolUse
        // — handlers see the executed result's context but their return
        // value is ignored.
        tool_funnel
            .post_tool_use(
                name,
                arguments.clone(),
                workspace.clone(),
                Some(agent_id.clone()),
                Some(session_id_owned.clone()),
                caller_id.map(str::to_string),
                Some(principal_id.to_string()),
                Some(principal_name.to_string()),
                capabilities.clone(),
                active_extensions.clone(),
            )
            .await;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        // F32a: propagate `success` into the persisted JSONL record and the
        // next-iteration LLM message so resumed sessions and the in-flight
        // context distinguish a failed dispatch from a successful zero-data
        // return. (Pre-F32a both sites hardcoded `is_error: false` even on
        // a failed tool execution — see audit doc section 3 row 2.)
        let is_error = !success;

        // Persist via SessionView trait port — locks the internal
        // RwLock inside the impl on Arc<RwLock<Session>> in root.
        session
            .add_tool_result(id, name, &tool_result_str, is_error)
            .await?;

        on_event(AgenticEvent::ToolEnd {
            run_id: run_id.to_string(),
            tool_id: id.clone(),
            result: tool_result_json,
            success,
            duration_ms,
        });

        let message =
            LlmMessage::tool_result(id.clone(), name.clone(), tool_result_str.clone(), is_error)
                .with_tool_call_id(id.clone());

        Ok(ToolExecutionResult { message, success })
    }
}

#[cfg(test)]
mod tests {
    use super::IdenticalFailureBreaker;

    #[test]
    fn breaker_allows_first_two_failures_then_refuses() {
        let b = IdenticalFailureBreaker::default();
        let args = serde_json::json!({"at": "2020-01-01T00:00:00Z"});
        assert!(b.refusal("CronCreate", &args).is_none());
        b.record("CronCreate", &args, false);
        assert!(b.refusal("CronCreate", &args).is_none());
        b.record("CronCreate", &args, false);
        let refusal = b.refusal("CronCreate", &args).expect("third call refused");
        assert!(refusal.contains("CronCreate"));
        assert!(refusal.contains("already failed 2 times"));
    }

    #[test]
    fn success_resets_the_counter() {
        let b = IdenticalFailureBreaker::default();
        let args = serde_json::json!({"x": 1});
        b.record("Bash", &args, false);
        b.record("Bash", &args, false);
        b.record("Bash", &args, true);
        assert!(b.refusal("Bash", &args).is_none());
    }

    #[test]
    fn different_args_are_independent() {
        let b = IdenticalFailureBreaker::default();
        let a = serde_json::json!({"x": 1});
        let c = serde_json::json!({"x": 2});
        b.record("Bash", &a, false);
        b.record("Bash", &a, false);
        assert!(b.refusal("Bash", &a).is_some());
        assert!(b.refusal("Bash", &c).is_none());
    }

    #[test]
    fn different_tools_are_independent() {
        let b = IdenticalFailureBreaker::default();
        let args = serde_json::json!({});
        b.record("CronCreate", &args, false);
        b.record("CronCreate", &args, false);
        assert!(b.refusal("CronList", &args).is_none());
    }
}
