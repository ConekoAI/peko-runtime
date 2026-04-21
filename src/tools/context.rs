//! Tool context for execution with abort signals and progress callbacks
//!
//! This module provides the infrastructure for:
//! - Aborting long-running tools via `AbortSignal`
//! - Progress updates during tool execution
//! - Tool monitoring and visibility
//! - Optional timeout handling

use crate::engine::AgenticEvent;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

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

use std::time::Duration;

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
    event_tx: Option<mpsc::Sender<AgenticEvent>>,
    /// Abort signal receiver
    abort_rx: tokio::sync::watch::Receiver<bool>,
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
    /// Peer identifier for distributed contexts
    pub peer_id: Option<String>,
    /// Workspace path
    pub workspace: Option<String>,
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
            progress_throttle_ms: 500, // Default 500ms between progress updates
            last_progress_update: Arc::new(tokio::sync::Mutex::new(None)),
            timeout: None,
            agent_id: None,
            session_id: None,
            peer_id: None,
            workspace: None,
        }
    }

    /// Create a minimal tool context for use when no abort/progress is needed.
    ///
    /// Used by `BuiltinToolAdapter` to ensure `execute_with_context` gets
    /// consistent metrics/timeout handling even when invoked through the hook system.
    pub fn default_for_tool(tool_name: impl Into<String>) -> Self {
        let (_tx, abort_rx) = tokio::sync::watch::channel(false);
        Self {
            run_id: "hook".to_string(),
            tool_id: "hook".to_string(),
            tool_name: tool_name.into(),
            event_tx: None,
            abort_rx,
            progress_throttle_ms: 500,
            last_progress_update: Arc::new(tokio::sync::Mutex::new(None)),
            timeout: None,
            agent_id: None,
            session_id: None,
            peer_id: None,
            workspace: None,
        }
    }

    /// Create a new tool context with event channel
    pub fn with_events(
        run_id: impl Into<String>,
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
        abort_rx: tokio::sync::watch::Receiver<bool>,
        event_tx: mpsc::Sender<AgenticEvent>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            tool_id: tool_id.into(),
            tool_name: tool_name.into(),
            event_tx: Some(event_tx),
            abort_rx,
            progress_throttle_ms: 500,
            last_progress_update: Arc::new(tokio::sync::Mutex::new(None)),
            timeout: None,
            agent_id: None,
            session_id: None,
            peer_id: None,
            workspace: None,
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

    /// Check if the tool execution has been aborted
    ///
    /// Tools should call this periodically during long-running operations
    /// and return early if aborted.
    ///
    /// # Example
    /// ```rust,ignore
    /// async fn execute_with_context(&self, params: Value, ctx: &ToolContext) -> Result<Value> {
    ///     for i in 0..100 {
    ///         if ctx.is_aborted() {
    ///             return Err(ToolError::Aborted.into());
    ///         }
    ///         // Do work...
    ///         ctx.report_progress(i, 100, Some(format!("Processing item {}", i))).await;
    ///     }
    ///     Ok(Value::Null)
    /// }
    /// ```
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
    ///
    /// # Arguments
    /// * `current` - Current progress value
    /// * `total` - Total progress value
    /// * `message` - Optional status message
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

            let event = AgenticEvent::ToolUpdate {
                run_id: self.run_id.clone(),
                tool_id: self.tool_id.clone(),
                output,
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
            let event = AgenticEvent::ToolUpdate {
                run_id: self.run_id.clone(),
                tool_id: self.tool_id.clone(),
                output: message.into(),
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
        event_tx: mpsc::Sender<AgenticEvent>,
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

/// Wrapper for tools that adds abort signal support
///
/// Similar to `OpenClaw`'s `wrapToolWithAbortSignal`, this wrapper
/// intercepts tool execution and passes an abort signal to the tool.
pub struct AbortableTool<T: ToolWithContext> {
    abort_signal: AbortSignal,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: ToolWithContext> AbortableTool<T> {
    /// Create a new abortable tool wrapper
    pub fn new(_inner: T) -> Self {
        Self {
            abort_signal: AbortSignal::new(),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Get the abort signal for this tool
    #[must_use]
    pub fn abort_signal(&self) -> AbortSignal {
        self.abort_signal.clone()
    }

    /// Abort the tool execution
    pub fn abort(&self) {
        self.abort_signal.abort();
    }
}

/// Trait for tools that support context-aware execution
///
/// This is an extension of the base `Tool` trait that adds support for:
/// - Abort signals
/// - Progress updates
/// - Timeout handling
#[async_trait::async_trait]
pub trait ToolWithContext: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> String;

    /// Execute with full context (abort signal + progress callbacks)
    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value>;

    /// Check if this tool supports progress updates
    fn supports_progress(&self) -> bool {
        true
    }
}

/// Adapter to wrap a basic Tool as a `ToolWithContext`
pub struct ToolAdapter<T> {
    inner: T,
}

impl<T: super::Tool> ToolAdapter<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl<T: super::Tool + Send + Sync> ToolWithContext for ToolAdapter<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> String {
        self.inner.description()
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        // Check abort before starting
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout before starting
        let start_time = std::time::Instant::now();
        ctx.check_timeout(start_time)?;

        // Delegate to the inner Tool::execute.
        // This is a trait adapter, not a production execution path.
        let result = self.inner.execute(params).await;

        // Check abort after completion
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout after completion
        ctx.check_timeout(start_time)?;

        result
    }

    fn supports_progress(&self) -> bool {
        false
    }
}

/// Wrap a basic tool with abort signal support
///
/// This is the Pekobot equivalent of `OpenClaw`'s `wrapToolWithAbortSignal`.
/// It returns a tuple of (`AbortableTool`, `AbortSignal`) where the signal
/// can be used to abort the tool from outside.
pub fn wrap_tool<T: ToolWithContext>(tool: T) -> (AbortableTool<T>, AbortSignal) {
    let abortable = AbortableTool::new(tool);
    let signal = abortable.abort_signal();
    (abortable, signal)
}

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
            match event {
                AgenticEvent::ToolUpdate {
                    progress_percent,
                    output,
                    ..
                } => {
                    assert_eq!(progress_percent, Some(50));
                    assert_eq!(output, "Half done");
                }
                _ => panic!("Expected ToolUpdate event"),
            }
        } else {
            panic!("No event received");
        }
    }
}
