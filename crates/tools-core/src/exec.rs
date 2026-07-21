//! Tool execution primitives for the Extension framework
//!
//! This module provides the fundamental types used during tool execution:
//! - `ToolContext`: execution context with abort signals and progress reporting
//! - `ToolError`: error types for tool execution
//! - `AbortSignal`: sender side of the abort mechanism
//! - `ToolResult`: structured result of a tool execution
//! - `ToolWithContext`: marker trait for context-aware tools
//!
//! # Module Boundary Note
//!
//! These primitives live in `tools::core` and are the canonical home. There
//! are no re-exports anywhere else in the crate.

use super::context_source::ContextSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Progress event emitted by a tool during execution.
///
/// Framework-defined; intentionally independent of `crate::engine::AgenticEvent`
/// so the extension framework does not depend on the engine. Callers that want
/// to surface tool progress in the engine's event stream should convert this
/// to an `AgenticEvent::ToolUpdate` at the integration boundary.
#[derive(Debug, Clone)]
pub struct ToolProgressEvent {
    /// Run identifier
    pub run_id: String,
    /// Tool identifier
    pub tool_id: String,
    /// Optional incremental output to surface
    pub output: Option<String>,
    /// Optional progress percentage (0-100)
    pub progress_percent: Option<u8>,
}

/// Errors that can occur during tool execution
#[derive(Debug, Clone, PartialEq)]
pub enum ToolError {
    /// Tool execution was aborted
    Aborted,
    /// Tool execution timed out
    Timeout(Duration),
    /// Other error
    Other(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::Aborted => write!(f, "Tool execution aborted"),
            ToolError::Timeout(d) => write!(f, "Tool execution timed out after {d:?}"),
            ToolError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        let msg = e.to_string();
        if msg.contains("aborted") {
            ToolError::Aborted
        } else if msg.contains("timeout") {
            ToolError::Other(msg) // Timeout has its own variant, but parse from string if needed
        } else {
            ToolError::Other(msg)
        }
    }
}

/// Context passed to tools during execution
///
/// Provides tools with:
/// - Ability to check if execution should be aborted
/// - Ability to emit progress updates (with throttling)
/// - Optional timeout handling
/// - Agent/session identity for reserved parameter injection
#[derive(Clone)]
pub struct ToolContext {
    /// Run identifier for this execution
    pub run_id: String,
    /// Tool execution identifier
    pub tool_id: String,
    /// Tool name
    pub tool_name: String,
    /// Channel for emitting events (progress updates, etc.)
    event_tx: Option<mpsc::Sender<ToolProgressEvent>>,
    /// Abort signal receiver
    abort_rx: tokio::sync::watch::Receiver<bool>,
    /// Sender half of the abort channel, kept alive so the watch
    /// channel doesn't close (and `rx.changed().await` doesn't return
    /// `Err` immediately, which would make `wait_for_abort` fire
    /// spuriously). Set by constructors that synthesize a fresh
    /// abort channel (`for_hook_run`, `default_for_tool`); `None`
    /// when the channel was sourced from an external `AbortSignal`
    /// (which holds its own keeper in `AbortSignal._rx_keeper`).
    _abort_tx_keeper: Option<tokio::sync::watch::Sender<bool>>,
    /// Progress update throttle (minimum ms between updates)
    pub progress_throttle_ms: u64,
    /// Last progress update time (for throttling)
    last_progress_update: Arc<tokio::sync::Mutex<Option<Instant>>>,
    /// Optional timeout for this tool execution
    pub timeout: Option<Duration>,
    /// Agent identifier (for reserved parameter injection)
    pub agent_id: Option<String>,
    /// Session identifier (for reserved parameter injection)
    pub session_id: Option<String>,
    /// Subject identifier for distributed contexts
    pub peer_id: Option<String>,
    /// Workspace path
    pub workspace: Option<String>,
    /// Spawning principal runtime id (post-PR-#94). Extension-scoped
    /// tools use this to resolve per-principal state at handle time.
    pub principal_id: Option<String>,
    /// Human-readable Principal name. Cron-scoped tools use this to
    /// create and filter jobs for the current Principal.
    pub principal_name: Option<String>,
    /// Principal capability grants carried with the tool call.
    /// Used by extension-scoped tools (e.g. `Skill`) to decide whether
    /// the requested entity is enabled.
    pub capabilities: Option<Vec<String>>,
    /// IDs of extensions active for the current principal.
    pub active_extensions: Option<Vec<String>>,
}

impl ToolContext {
    /// Create a new tool context
    pub fn new(
        run_id: impl Into<String>,
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
        abort_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            tool_id: tool_id.into(),
            tool_name: tool_name.into(),
            event_tx: None,
            abort_rx,
            // External `abort_rx` — assumed to come from an `AbortSignal`
            // (which holds its own keeper) or an equivalent external owner.
            _abort_tx_keeper: None,
            progress_throttle_ms: 500, // Default 500ms between progress updates
            last_progress_update: Arc::new(tokio::sync::Mutex::new(None)),
            timeout: None,
            agent_id: None,
            session_id: None,
            peer_id: None,
            workspace: None,
            principal_id: None,
            principal_name: None,
            capabilities: None,
            active_extensions: None,
        }
    }

    /// Create a minimal tool context for use when no abort/progress is needed.
    ///
    /// Used by `BuiltinToolAdapter` to ensure `execute_with_context` gets
    /// consistent metrics/timeout handling even when invoked through the hook system.
    pub fn default_for_tool(tool_name: impl Into<String>) -> Self {
        let (tx, abort_rx) = tokio::sync::watch::channel(false);
        Self {
            run_id: "hook".to_string(),
            tool_id: "hook".to_string(),
            tool_name: tool_name.into(),
            event_tx: None,
            abort_rx,
            // We synthesized the channel — keep the sender so the
            // channel doesn't close and `rx.changed().await` doesn't
            // return `Err` immediately.
            _abort_tx_keeper: Some(tx),
            progress_throttle_ms: 500,
            last_progress_update: Arc::new(tokio::sync::Mutex::new(None)),
            timeout: None,
            agent_id: None,
            session_id: None,
            peer_id: None,
            workspace: None,
            principal_id: None,
            principal_name: None,
            capabilities: None,
            active_extensions: None,
        }
    }

    /// Create a tool context for hook-based execution without an external abort signal.
    ///
    /// This is used by the extension framework when injecting tool context into
    /// hook invocations for reserved parameter resolution.
    pub fn for_hook_run(
        run_id: impl Into<String>,
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        let (tx, abort_rx) = tokio::sync::watch::channel(false);
        Self {
            run_id: run_id.into(),
            tool_id: tool_id.into(),
            tool_name: tool_name.into(),
            event_tx: None,
            abort_rx,
            // We synthesized the channel — keep the sender so it doesn't close.
            _abort_tx_keeper: Some(tx),
            progress_throttle_ms: 500,
            last_progress_update: Arc::new(tokio::sync::Mutex::new(None)),
            timeout: None,
            agent_id: None,
            session_id: None,
            peer_id: None,
            workspace: None,
            principal_id: None,
            principal_name: None,
            capabilities: None,
            active_extensions: None,
        }
    }

    /// Create a tool context for hook-based execution that observes an
    /// external `watch::Receiver<bool>` abort signal.
    ///
    /// Unlike [`Self::for_hook_run`] (which produces a fresh never-aborted
    /// `abort_rx` and therefore makes the trait-default
    /// `ctx.is_aborted()` check a no-op), this constructor lets the
    /// engine thread a real abort signal through to the tool. Used by
    /// `BuiltinToolAdapter` when the engine has built an abort bridge
    /// from a `CancellationToken` (see
    /// [`bridge_from_cancellation_token`]).
    pub fn for_hook_run_with_abort(
        run_id: impl Into<String>,
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
        abort_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        let mut ctx = Self::for_hook_run(run_id, tool_id, tool_name);
        ctx.abort_rx = abort_rx;
        // The receiver came from an external `AbortSignal` (which
        // holds its own keeper). Drop the synthesized keeper that
        // `for_hook_run` installed — keeping both would hold a
        // phantom sender that would prevent the channel from ever
        // closing when the engine-side owner goes away.
        ctx._abort_tx_keeper = None;
        ctx
    }

    /// Create a new tool context with event channel
    pub fn with_events(
        run_id: impl Into<String>,
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
        abort_rx: tokio::sync::watch::Receiver<bool>,
        event_tx: mpsc::Sender<ToolProgressEvent>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            tool_id: tool_id.into(),
            tool_name: tool_name.into(),
            event_tx: Some(event_tx),
            abort_rx,
            // External `abort_rx` — no keeper needed here.
            _abort_tx_keeper: None,
            progress_throttle_ms: 500,
            last_progress_update: Arc::new(tokio::sync::Mutex::new(None)),
            timeout: None,
            agent_id: None,
            session_id: None,
            peer_id: None,
            workspace: None,
            principal_id: None,
            principal_name: None,
            capabilities: None,
            active_extensions: None,
        }
    }

    /// Set progress throttle duration
    #[must_use]
    pub fn with_throttle(mut self, ms: u64) -> Self {
        self.progress_throttle_ms = ms;
        self
    }

    /// Set timeout for this tool execution
    #[must_use]
    pub fn with_timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Set agent ID for reserved parameter injection
    #[must_use]
    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Set session ID for reserved parameter injection
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set peer ID for distributed contexts
    #[must_use]
    pub fn with_peer_id(mut self, peer_id: impl Into<String>) -> Self {
        self.peer_id = Some(peer_id.into());
        self
    }

    /// Set workspace path
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Set all identity fields at once for reserved parameter injection
    #[must_use]
    pub fn with_identity(
        mut self,
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        self.agent_id = Some(agent_id.into());
        self.session_id = Some(session_id.into());
        self.workspace = Some(workspace.into());
        self
    }

    /// Set principal id for extension-scoped tool state resolution
    #[must_use]
    pub fn with_principal_id(mut self, principal_id: impl Into<String>) -> Self {
        self.principal_id = Some(principal_id.into());
        self
    }

    /// Set capability grants for extension-scoped tool state resolution.
    #[must_use]
    pub fn with_capabilities(
        mut self,
        capabilities: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.capabilities = Some(capabilities.into_iter().map(Into::into).collect());
        self
    }

    /// Set active extension IDs for extension-scoped tool state resolution.
    #[must_use]
    pub fn with_active_extensions(
        mut self,
        active_extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.active_extensions = Some(active_extensions.into_iter().map(Into::into).collect());
        self
    }

    /// Set principal name for Principal-scoped tools (e.g. cron).
    #[must_use]
    pub fn with_principal_name(mut self, principal_name: impl Into<String>) -> Self {
        self.principal_name = Some(principal_name.into());
        self
    }

    /// Replace the abort receiver. Use when the engine built a fresh
    /// context via [`Self::for_hook_run`] (a never-aborted receiver)
    /// and then later needs to attach a real `CancellationToken` bridge
    /// (e.g. the closure in `BuiltinToolAdapter` which can't move out of
    /// `for_hook_run`'s receiver because the rest of the builder chain
    /// has already run on top of it).
    #[must_use]
    pub fn with_abort_signal(mut self, abort_rx: tokio::sync::watch::Receiver<bool>) -> Self {
        self.abort_rx = abort_rx;
        self
    }

    /// Check if the tool execution has been aborted
    ///
    /// Tools should call this periodically during long-running operations
    /// and return early if aborted.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        *self.abort_rx.borrow()
    }

    /// Subscribe to abort signal changes
    ///
    /// Useful for tools that want to await on abort rather than poll
    #[must_use]
    pub fn abort_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.abort_rx.clone()
    }

    /// Check if enough time has passed since last progress update
    async fn should_send_progress(&self) -> bool {
        if self.progress_throttle_ms == 0 {
            return true; // No throttling
        }

        let mut last = self.last_progress_update.lock().await;
        let now = Instant::now();

        if let Some(last_time) = *last {
            let elapsed = now.duration_since(last_time).as_millis() as u64;
            if elapsed >= self.progress_throttle_ms {
                *last = Some(now);
                true
            } else {
                false
            }
        } else {
            *last = Some(now);
            true
        }
    }

    /// Report progress during tool execution
    ///
    /// Emits a `ToolUpdate` event if an event channel is configured
    /// and the throttle interval has passed. If throttled, the update
    /// is silently dropped.
    pub async fn report_progress(&self, current: usize, total: usize, message: Option<String>) {
        if self.event_tx.is_none() {
            return;
        }

        // Check throttle
        if !self.should_send_progress().await {
            return;
        }

        if let Some(ref tx) = self.event_tx {
            let percent = if total > 0 {
                Some((current * 100 / total) as u8)
            } else {
                None
            };

            let output = message.unwrap_or_else(|| format!("Progress: {current}/{total}"));

            let event = ToolProgressEvent {
                run_id: self.run_id.clone(),
                tool_id: self.tool_id.clone(),
                output: Some(output),
                progress_percent: percent,
            };

            // Don't fail if channel is closed
            let _ = tx.send(event).await;
        }
    }

    /// Report a progress message without percentage
    ///
    /// Status updates are also throttled to avoid flooding.
    pub async fn report_status(&self, message: impl Into<String>) {
        if self.event_tx.is_none() {
            return;
        }

        // Check throttle
        if !self.should_send_progress().await {
            return;
        }

        if let Some(ref tx) = self.event_tx {
            let event = ToolProgressEvent {
                run_id: self.run_id.clone(),
                tool_id: self.tool_id.clone(),
                output: Some(message.into()),
                progress_percent: None,
            };
            let _ = tx.send(event).await;
        }
    }

    /// Get the configured timeout
    #[must_use]
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Check if the tool has exceeded its timeout
    ///
    /// Returns `Some(elapsed)` if timed out, `None` if still within limit
    pub fn check_timeout(&self, start_time: Instant) -> Result<(), ToolError> {
        if let Some(timeout) = self.timeout {
            let elapsed = start_time.elapsed();
            if elapsed > timeout {
                return Err(ToolError::Timeout(timeout));
            }
        }
        Ok(())
    }
}

/// Abort signal for tool execution
///
/// This is the sender side of the abort mechanism. When `abort()` is called,
/// all tools checking the corresponding `ToolContext` will see `is_aborted() == true`.
#[derive(Clone)]
pub struct AbortSignal {
    tx: tokio::sync::watch::Sender<bool>,
}

impl AbortSignal {
    /// Create a new abort signal (initially not aborted)
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        Self { tx }
    }

    /// Abort the tool execution
    pub fn abort(&self) {
        let _ = self.tx.send(true);
    }

    /// Check if already aborted
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        *self.tx.borrow()
    }

    /// Create a tool context with this abort signal
    pub fn create_context(
        &self,
        run_id: impl Into<String>,
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> ToolContext {
        ToolContext::new(run_id, tool_id, tool_name, self.tx.subscribe())
    }

    /// Create a tool context with event channel
    pub fn create_context_with_events(
        &self,
        run_id: impl Into<String>,
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
        event_tx: mpsc::Sender<ToolProgressEvent>,
    ) -> ToolContext {
        ToolContext::with_events(run_id, tool_id, tool_name, self.tx.subscribe(), event_tx)
    }

    /// Get a receiver for the abort signal
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<bool> {
        self.tx.subscribe()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard that aborts the spawned bridge task on drop.
///
/// Returned by [`bridge_from_cancellation_token`] alongside the
/// `AbortSignal`. The guard's `Drop` impl aborts the bridge task so it
/// can't outlive the tool call that owns the abort signal — preventing
/// a `send(true)` against a `watch::Sender` whose receiver is gone.
///
/// Also holds an internal `watch::Receiver` keeper so the watch
/// channel stays open until the guard drops. This is the race window
/// between "bridge task fires" and "tool body subscribes via
/// `ToolContext::abort_signal`" — without the keeper, the bridge's
/// `send(true)` would `Err(SendError)` and the abort would silently
/// fail. Once the tool body has its own subscriber, dropping the
/// guard closes the keeper and the channel can clean up.
pub struct AbortSignalBridgeGuard {
    handle: Option<tokio::task::JoinHandle<()>>,
    /// Keeper for the watch channel; dropped alongside the guard.
    /// Field is `Option` so `Drop` can `.take()` it before the
    /// generated `Drop` runs.
    _rx_keeper: Option<tokio::sync::watch::Receiver<bool>>,
}

impl AbortSignalBridgeGuard {
    /// No-op guard for paths that didn't actually spawn a bridge task
    /// (e.g. `cancel: None`). Holding a `noop()` guard keeps call sites
    /// uniform — they always destructure `(signal, guard)` and let the
    /// guard drop normally at scope end.
    #[must_use]
    pub const fn noop() -> Self {
        Self {
            handle: None,
            _rx_keeper: None,
        }
    }
}

impl Drop for AbortSignalBridgeGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Build an `AbortSignal` that fires when `cancel` is cancelled.
///
/// Spawns a small `tokio` task that awaits `cancel.cancelled()` and
/// then calls `signal.abort()`. The returned [`AbortSignalBridgeGuard`]
/// must be kept alive for the lifetime of the tool call — when dropped
/// it aborts the bridge task to prevent it from outliving the
/// `ToolContext` that subscribes to the `AbortSignal`.
///
/// This is the bridge between the engine's `CancellationToken` (the
/// hierarchical soft-interrupt primitive) and the `watch::Receiver<bool>`
/// that `ToolContext` exposes. It is intentionally a one-line bridge
/// (not a unification) so that the existing `AbortSignal` API stays
/// intact for extension authors and the trait-default
/// `ctx.is_aborted()` check (`src/tools/core/traits.rs:82, 102`) starts
/// working in production the moment the engine supplies a real
/// `abort_rx` via [`ToolContext::for_hook_run_with_abort`].
#[must_use]
pub fn bridge_from_cancellation_token(
    cancel: tokio_util::sync::CancellationToken,
) -> (AbortSignal, AbortSignalBridgeGuard) {
    let (tx, rx_keeper) = tokio::sync::watch::channel(false);
    let tx2 = tx.clone();
    let handle = tokio::spawn(async move {
        cancel.cancelled().await;
        let _ = tx2.send(true);
    });
    (
        AbortSignal { tx },
        AbortSignalBridgeGuard {
            handle: Some(handle),
            // Race-window keeper: see struct docs. Holds the watch
            // channel open until the tool call that owns this guard
            // ends, so the bridge's `send(true)` doesn't fail with
            // `SendError` if it races ahead of the tool body
            // subscribing via `ToolContext::abort_signal`.
            _rx_keeper: Some(rx_keeper),
        },
    )
}

/// RAII guard that aborts the spawned reverse-bridge task on drop.
///
/// Returned by [`bridge_to_cancellation_token`] alongside the
/// `CancellationToken`. The guard's `Drop` impl aborts the bridge
/// task so it can't outlive the tool call that owns the abort signal.
pub struct CancellationTokenBridgeGuard {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl CancellationTokenBridgeGuard {
    /// No-op guard for the `rx = None` path.
    #[must_use]
    pub const fn noop() -> Self {
        Self { handle: None }
    }
}

impl Drop for CancellationTokenBridgeGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Build a `CancellationToken` that fires when the watch receiver
/// flips to `true`.
///
/// This is the **reverse** of [`bridge_from_cancellation_token`]: a
/// tool that holds a `watch::Receiver<bool>` (e.g. from
/// [`ToolContext::abort_signal`]) can derive a `CancellationToken` to
/// hand to a downstream component that expects a token — most
/// importantly, `AgentTool` passes this token to the sub-agent's
/// `AgenticLoop` so a parent cancel propagates into a spawned
/// sub-agent. Without this, the sub-agent's loop would never observe
/// the parent's interrupt.
///
/// The returned [`CancellationTokenBridgeGuard`] must be kept alive
/// for the lifetime of the tool call so the spawned task is aborted
/// on drop. `rx = None` yields a never-cancelled token and a no-op
/// guard — useful for legacy call sites that don't have an abort
/// receiver.
#[must_use]
pub fn bridge_to_cancellation_token(
    rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> (
    tokio_util::sync::CancellationToken,
    CancellationTokenBridgeGuard,
) {
    let mut rx = match rx {
        Some(rx) => rx,
        None => {
            return (
                tokio_util::sync::CancellationToken::new(),
                CancellationTokenBridgeGuard::noop(),
            );
        }
    };
    let token = tokio_util::sync::CancellationToken::new();
    let token2 = token.clone();
    let handle = tokio::spawn(async move {
        // If the watch is already true at the moment we register
        // (caller signaled before the bridge spawned), flip the
        // token immediately.
        if *rx.borrow() {
            token2.cancel();
            return;
        }
        loop {
            if rx.changed().await.is_err() {
                return;
            }
            if *rx.borrow() {
                token2.cancel();
                return;
            }
        }
    });
    (
        token,
        CancellationTokenBridgeGuard {
            handle: Some(handle),
        },
    )
}

/// Adapter that implements `ContextSource` for `ToolContext`
///
/// This bridges the tool execution primitives to the extension framework's
/// context resolver for reserved parameter injection.
pub struct ToolContextAdapter<'a> {
    ctx: &'a ToolContext,
}

impl<'a> ToolContextAdapter<'a> {
    #[must_use]
    pub fn new(ctx: &'a ToolContext) -> Self {
        Self { ctx }
    }
}

impl ContextSource for ToolContextAdapter<'_> {
    fn get_session_id(&self) -> Option<String> {
        self.ctx.session_id.clone()
    }

    fn get_agent_id(&self) -> Option<String> {
        self.ctx.agent_id.clone()
    }

    fn get_peer_id(&self) -> Option<String> {
        self.ctx.peer_id.clone()
    }

    fn get_workspace(&self) -> Option<String> {
        self.ctx.workspace.clone()
    }

    fn get_run_id(&self) -> Option<String> {
        Some(self.ctx.run_id.clone())
    }
}

/// Trait for tools that support context-aware execution
///
/// This is a marker trait for types that are natively context-aware.
/// All `Tool` implementations already have `execute_with_context` via
/// the default implementation on the `Tool` trait.
#[async_trait::async_trait]
pub trait ToolWithContext: Send + Sync {
    /// Check if this tool supports progress updates
    fn supports_progress(&self) -> bool {
        true
    }
}

// Blanket impl that was deliberately omitted from extensions/framework
// (the cycle prevented it). Now that `Tool` lives in `peko-tools-core`
// alongside `ToolWithContext`, the impl is sound.
impl<T: crate::Tool> ToolWithContext for T {}

/// Result of a tool execution
///
/// This is a structured result that can represent success or failure,
/// with optional metadata for async tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    /// Whether the tool execution succeeded
    pub success: bool,
    /// The result data (if success)
    pub data: Option<Value>,
    /// Error message (if failure)
    pub error: Option<String>,
    /// Optional metadata
    pub metadata: Option<Value>,
}

impl ToolResult {
    /// Create a successful tool result
    pub fn success(data: impl Into<Value>) -> Self {
        Self {
            success: true,
            data: Some(data.into()),
            error: None,
            metadata: None,
        }
    }

    /// Create a failed tool result
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error.into()),
            metadata: None,
        }
    }

    /// Create a failed tool result with a standard error
    #[must_use]
    pub fn error(err: anyhow::Error) -> Self {
        Self::failure(err.to_string())
    }

    /// Add metadata to the result
    pub fn with_metadata(mut self, metadata: impl Into<Value>) -> Self {
        self.metadata = Some(metadata.into());
        self
    }

    /// Convert to JSON value for LLM consumption
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "success": false,
                "error": "Failed to serialize tool result"
            })
        })
    }
}

// The `impl From<ToolResult> for HookOutput` (and `tool_result_from_hook`
// shim) used to live here. It has been migrated to
// `extensions::framework::types::hook_io` because the orphan rule
// requires trait impls that touch a foreign type to be in the crate
// owning at least one of {trait, type}; `ToolResult` now lives in
// `peko-tools-core` while `HookOutput` lives in the extensions
// crate, so the impl is in the extensions crate next to `HookOutput`.

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_abort_signal() {
        let signal = AbortSignal::new();
        let ctx = signal.create_context("run-1", "tool-1", "test-tool");

        assert!(!ctx.is_aborted());
        assert!(!signal.is_aborted());

        signal.abort();

        assert!(ctx.is_aborted());
        assert!(signal.is_aborted());
    }

    #[tokio::test]
    async fn test_tool_error_display() {
        assert_eq!(ToolError::Aborted.to_string(), "Tool execution aborted");
        assert_eq!(
            ToolError::Timeout(Duration::from_secs(30)).to_string(),
            "Tool execution timed out after 30s"
        );
        assert_eq!(
            ToolError::Other("something failed".to_string()).to_string(),
            "something failed"
        );
    }

    #[tokio::test]
    async fn test_progress_throttling() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let signal = AbortSignal::new();
        let ctx = signal
            .create_context_with_events("run-1", "tool-1", "test", tx)
            .with_throttle(100); // 100ms throttle

        // First update should go through
        ctx.report_progress(10, 100, Some("10%".to_string())).await;

        // Second update immediately should be throttled
        ctx.report_progress(20, 100, Some("20%".to_string())).await;

        // Wait for throttle window
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        // Third update should go through
        ctx.report_progress(30, 100, Some("30%".to_string())).await;

        // Count events
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }

        assert_eq!(count, 2, "Should have received 2 events (1st and 3rd)");
    }

    #[tokio::test]
    async fn test_timeout() {
        let signal = AbortSignal::new();
        let ctx = signal
            .create_context("run-1", "tool-1", "test")
            .with_timeout(Duration::from_millis(100));

        let start = Instant::now();

        // Should not timeout immediately
        assert!(ctx.check_timeout(start).is_ok());

        // Wait past timeout
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        // Should now timeout
        let err = ctx.check_timeout(start).unwrap_err();
        assert!(matches!(err, ToolError::Timeout(_)));
    }

    #[tokio::test]
    async fn test_progress_reporting() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let signal = AbortSignal::new();
        let ctx = signal.create_context_with_events("run-1", "tool-1", "test", tx);

        ctx.report_progress(50, 100, Some("Half done".to_string()))
            .await;

        if let Some(event) = rx.recv().await {
            assert!(
                matches!(event.progress_percent, Some(50)),
                "expected progress_percent=Some(50), got {:?}",
                event.progress_percent
            );
            assert_eq!(event.output, Some("Half done".to_string()));
        } else {
            panic!("No event received");
        }
    }

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success(serde_json::json!({"key": "value"}));
        assert!(result.success);
        assert_eq!(result.data, Some(serde_json::json!({"key": "value"})));
        assert!(result.error.is_none());
    }

    #[test]
    fn test_tool_result_failure() {
        let result = ToolResult::failure("something went wrong");
        assert!(!result.success);
        assert!(result.data.is_none());
        assert_eq!(result.error, Some("something went wrong".to_string()));
    }

    #[test]
    fn test_tool_result_to_json() {
        let result = ToolResult::success(42).with_metadata(serde_json::json!({"time": 1}));
        let json = result.to_json();
        assert_eq!(json["success"], true);
        assert_eq!(json["data"], 42);
        assert_eq!(json["metadata"], serde_json::json!({"time": 1}));
    }

    /// Verifies the `CancellationToken → AbortSignal` bridge fires the
    /// underlying `AbortSignal` when the token is cancelled. Pre-merge
    /// check for the "interrupt actually means stop" follow-up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bridge_fires_on_cancel() {
        let token = tokio_util::sync::CancellationToken::new();
        let (signal, _guard) = bridge_from_cancellation_token(token.clone());

        // Pre-cancel: signal reports not-aborted.
        assert!(!signal.is_aborted());

        // Cancel the token; the bridge task should flip the signal
        // within a few ms. Poll instead of sleeping once so the
        // assertion is robust to runtime scheduling jitter.
        token.cancel();
        for _ in 0..50 {
            if signal.is_aborted() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(signal.is_aborted(), "AbortSignal must fire on token cancel");
    }

    /// Verifies that the `AbortSignalBridgeGuard` aborts the bridge
    /// task on drop. We assert the **observable effect** of an aborted
    /// task: cancelling the underlying token *after* the guard drops
    /// does not flip the signal. If the bridge task were still
    /// running, the cancel would still propagate to the signal (the
    /// bridge would re-fire) — so the assertion is meaningful.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bridge_drop_aborts_spawned_task() {
        let token = tokio_util::sync::CancellationToken::new();
        let (signal, guard) = bridge_from_cancellation_token(token.clone());

        // Drop the guard first — the bridge task is aborted.
        drop(guard);
        // Now cancel the token. The aborted task can no longer
        // observe this and `send(true)` it to the signal.
        token.cancel();
        for _ in 0..50 {
            // Give the runtime plenty of time to potentially re-fire
            // the bridge if it weren't really aborted.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if signal.is_aborted() {
                break;
            }
        }
        assert!(
            !signal.is_aborted(),
            "signal should remain false: aborted bridge task must not fire send"
        );
    }

    /// Verifies the **reverse** bridge (`watch::Receiver<bool> →
    /// CancellationToken`) flips the local token when the watch
    /// receiver signals. Used by `AgentTool` to derive a token from
    /// `ToolContext::abort_signal()` for the sub-agent's loop.
    #[tokio::test]
    async fn reverse_bridge_fires_on_watch() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let (token, _guard) = bridge_to_cancellation_token(Some(rx));
        assert!(!token.is_cancelled());

        // Flip the watch — the reverse bridge should fire the token.
        tx.send(true).unwrap();
        // Give the bridge task a chance to schedule.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            token.is_cancelled(),
            "CancellationToken must fire on watch flip"
        );
    }

    /// Verifies the `noop()` path of `bridge_to_cancellation_token`:
    /// when no watch is supplied, the returned token is never cancelled
    /// and the guard is a no-op (no task to abort).
    #[tokio::test]
    async fn reverse_bridge_noop_never_cancels() {
        let (token, _guard) = bridge_to_cancellation_token(None);
        assert!(!token.is_cancelled());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !token.is_cancelled(),
            "no-op bridge must never cancel the token"
        );
    }
}
