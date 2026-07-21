//! Tool trait

use crate::exec::{ToolContext, ToolError};
use crate::interrupt::ToolInterruptNotice;
use crate::ToolExposure;
use async_trait::async_trait;

/// Tool trait for agent capabilities
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    /// Get LLM-optimized description with usage guidance.
    ///
    /// This should include "Use when:" and "Don't use when:" guidance
    /// to help the LLM select the right tool.
    ///
    /// Example: "Execute terminal commands. Use when: running build/test commands,
    /// inspecting system state. Don't use when: a safer dedicated tool exists."
    fn description(&self) -> String;

    /// Convert to Any for downcasting
    ///
    /// This enables downcasting from `Arc<dyn Tool>` to concrete types
    /// for feature detection and trait implementation checking.
    fn as_any(&self) -> &dyn std::any::Any {
        // Default implementation panics - tools must override
        panic!("as_any not implemented for this tool")
    }

    /// Get the JSON Schema for this tool's parameters
    ///
    /// This is used for native tool calling APIs (`OpenAI`, Anthropic, etc.)
    /// Default implementation returns an empty object schema.
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    /// Whether multiple instances of this tool can run concurrently
    /// within a single LLM-response batch.
    ///
    /// Defaults to `true` (the common case for idempotent reads and
    /// pure-compute tools). Tools that mutate shared state should
    /// override to `false`:
    ///
    /// * Filesystem writes (`Write`, `Edit`) — two concurrent writes
    ///   clobber each other; `Read` racing with `Write` can observe a
    ///   half-written file.
    /// * Shell (`Bash`) — cwd, env, and child processes can collide.
    /// * DB/queue ops that aren't atomic (cron Create/Delete, task
    ///   Create/Update).
    ///
    /// At dispatch time the [`ToolExecutor`](crate::engine::tool_executor::ToolExecutor)
    /// takes a read-lock on the agent's [`ParallelGate`](crate::engine::parallel_gate::ParallelGate)
    /// when this returns `true` (concurrent dispatches OK) and a
    /// write-lock when `false` (exclusive against every other running
    /// tool, parallelizable or not). Mirrors codex's
    /// `codex-rs/core/src/tools/parallel.rs` shape.
    fn parallelizable(&self) -> bool {
        true
    }

    /// How this tool is exposed to the LLM (F34, audit section 3 row 4).
    ///
    /// Defaults to [`ToolExposure::Direct`] (visible in both the prompt
    /// "Available Tools" section AND the native LLM catalog;
    /// callable). Override for:
    ///
    /// * `DirectModelOnly` — schema is self-documenting; suppress the
    ///   prose entry to save prompt tokens.
    /// * `Deferred` — too large for the prompt; the model discovers it
    ///   via the synthetic `__tool_search` stub (F35). The stub returns
    ///   the tool's full `ToolDefinition` so the model can call it on
    ///   the next iteration.
    /// * `Hidden` — telemetry-only or sub-tool-of-other-tool; the
    ///   model never sees or invokes it.
    ///
    /// The capability gate still applies on top — a `DirectModelOnly`
    /// tool without the principal's `tool:<name>` grant is hidden from
    /// both surfaces regardless of this setting.
    fn exposure(&self) -> ToolExposure {
        ToolExposure::default()
    }

    /// Execute the tool with parameters.
    ///
    /// ⚠️ **TEST-ONLY IN PRODUCTION CONTEXTS**
    ///
    /// Production code must route tool execution through `ExtensionCore::invoke_hook`
    /// (or `ToolRuntime::execute_tool`) to ensure consistent behavior:
    /// - Workspace injection
    /// - Reserved parameter validation/injection
    /// - Tool permission checks (ADR-019)
    /// - Abort/timeout handling
    /// - Progress reporting
    /// - Metrics collection
    ///
    /// Direct calls to this method are appropriate for:
    /// - Unit tests of individual tools
    /// - The `BuiltinToolAdapter` wrapper (which bridges into ExtensionCore)
    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value>;

    /// Hook called by the framework when a tool call is cancelled.
    ///
    /// This is the **single seam** where a tool author expresses its interrupt
    /// semantics. Override it to do two things, in any order:
    ///
    /// 1. **Cleanup** of side-effects owned by this tool — kill spawned
    ///    subprocesses, roll back staged writes, drop network handles, abort
    ///    in-flight transactions, etc. Because `on_interrupt` runs with `&self`,
    ///    it has direct access to the tool's internal state (`Arc<Mutex<Inner>>`
    ///    fields, child handles, etc.) and can perform cleanup that the main
    ///    `execute` task can't do safely from inside itself.
    /// 2. **Describe** what happened by returning a [`ToolInterruptNotice`]
    ///    with the `preserved` / `rolled_back` / `leaked` / `resume_hint`
    ///    fields filled in for the calling agent.
    ///
    /// The default implementation does **no cleanup** and returns a soft
    /// default notice. Soft-path tools that just want the framework to emit a
    /// generic "cancelled" notice can leave this method alone.
    ///
    /// # What `on_interrupt` is *not*
    ///
    /// It is **not** the stop mechanism. The framework's existing abort
    /// plumbing — `ToolContext::abort_signal()` (a `watch::Receiver<bool>`),
    /// `bridge_to_cancellation_token`, the `tokio::select!` inside
    /// `BashTool::execute_command_blocking`, the child's
    /// `AgenticLoop::is_cancelled()` check — already stops long-running
    /// tools. By the time the framework calls `on_interrupt`, the abort
    /// signal has been flipped; the tool's `execute` is either returning
    /// promptly (because it polled `is_aborted()`) or finishing naturally
    /// (soft path). `on_interrupt` runs concurrently with that and gets to
    /// describe the aftermath.
    ///
    /// # Order of operations (cancel flow)
    ///
    /// ```text
    /// 1. Framework spawns the cancel watcher.
    /// 2. Framework calls `tool.execute_with_context(...)` (the tool's main work).
    /// 3. User (or upstream) flips the abort signal.
    /// 4. Watcher observes the flip; in parallel:
    ///    a. The tool's `execute` task observes `is_aborted()` and returns.
    ///    b. `on_interrupt` runs cleanup and returns the notice.
    /// 5. Framework emits the notice text on the next turn (cancel wins).
    /// ```
    ///
    /// Cleanup in `on_interrupt` therefore runs *concurrently* with the
    /// tool's `execute` task unwinding. Tools that need `execute` to have
    /// fully returned before they do cleanup should await an internal
    /// completion signal (e.g., a `tokio::sync::oneshot` their `execute`
    /// sends on when it observes `is_aborted()`).
    async fn on_interrupt(&self, tool_call_id: &str, ctx: &ToolContext) -> ToolInterruptNotice {
        ToolInterruptNotice::soft_default(tool_call_id, ctx.tool_name.as_str())
    }

    /// Execute with full context (abort signal + progress callbacks).
    ///
    /// This is the canonical execution method on the trait. The default implementation
    /// delegates to `execute` for backward compatibility, but tools that support progress
    /// reporting should override this.
    ///
    /// ⚠️ **TEST-ONLY IN PRODUCTION CONTEXTS** — see note on `execute`.
    ///
    /// # Arguments
    /// * `params` - Tool parameters from the LLM
    /// * `ctx` - Execution context with abort signal and progress callback
    ///
    /// # Returns
    /// Tool result or error (including abort errors)
    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        use std::time::Instant;

        // Check abort before starting
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout before starting
        let start_time = Instant::now();
        ctx.check_timeout(start_time)?;

        // Report start status
        ctx.report_status(format!("Starting {}", self.name())).await;

        // Execute using the basic method with timing captured for the status line
        let tool_name = self.name().to_string();
        let result = self.execute(params).await;
        let elapsed = start_time.elapsed();

        // Check abort after completion
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout after completion
        ctx.check_timeout(start_time)?;

        // Report completion
        ctx.report_status(format!("Completed {tool_name} in {elapsed:?}"))
            .await;

        result
    }

    /// Check if this tool supports progress updates
    ///
    /// Returns true if the tool implements custom progress reporting
    /// via `execute_with_context`. Default is false.
    fn supports_progress(&self) -> bool {
        false
    }

    /// Estimate execution duration for this tool call
    ///
    /// Returns an estimated duration in milliseconds.
    /// Used by the agent loop to decide whether to emit progress events.
    fn estimated_duration_ms(&self, _params: &serde_json::Value) -> u64 {
        1000 // Default 1 second
    }
}
