//! Background-execution hooks the host attaches to a tool call.
//!
//! Every background task runs through one path: the calling principal's
//! task executor. A tool that offers background mode (e.g. Bash
//! `run_in_background`) starts itself through [`BackgroundSpawner`]; the
//! host then runs the tool again as the task body, with
//! [`BackgroundContext::progress`] set so live output reaches the task.

use crate::exec::ToolContext;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

/// A request to run `tool` with `params` as a background task owned by
/// the caller's principal.
#[derive(Debug, Clone)]
pub struct BackgroundSpawn {
    pub tool: String,
    pub params: serde_json::Value,
    /// Task lifetime in milliseconds; `None` uses the executor default.
    pub timeout_millis: Option<u64>,
}

/// Starts background tasks for the calling principal.
#[async_trait]
pub trait BackgroundSpawner: Send + Sync {
    /// Register the task and return its id.
    async fn spawn(&self, request: BackgroundSpawn, ctx: &ToolContext) -> anyhow::Result<String>;
}

/// Background hooks for one tool call.
#[derive(Clone, Default)]
pub struct BackgroundContext {
    /// Set when this call is the body of a background task: write live
    /// output here, and run to completion inline (the task's own timeout
    /// and cancellation govern).
    pub progress: Option<Arc<Mutex<String>>>,
    /// Set when the caller's principal can start background tasks.
    pub spawner: Option<Arc<dyn BackgroundSpawner>>,
}

impl BackgroundContext {
    /// Whether this call is already running as a background task.
    #[must_use]
    pub fn is_task_body(&self) -> bool {
        self.progress.is_some()
    }
}
