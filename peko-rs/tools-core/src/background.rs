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

/// Cap on a task's live progress buffer. Once exceeded, the oldest bytes
/// are dropped: recent output is what matters when a caller inspects a
/// long-running task, and a chatty tool must not grow the buffer for its
/// whole lifetime.
pub const PROGRESS_MAX_BYTES: usize = 64 * 1024;

/// Append `chunk` to a task's progress buffer, keeping only the last
/// [`PROGRESS_MAX_BYTES`]. Task bodies write progress through this.
pub fn append_progress(progress: &Mutex<String>, chunk: &str) {
    let mut buf = progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    buf.push_str(chunk);
    let keep = tail(&buf, PROGRESS_MAX_BYTES).len();
    let drop = buf.len() - keep;
    buf.drain(..drop);
}

/// The last `max_bytes` of `s` or fewer, never splitting a character.
#[must_use]
pub fn tail(s: &str, max_bytes: usize) -> &str {
    let mut start = s.len().saturating_sub(max_bytes);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_never_splits_a_character() {
        assert_eq!(tail("hello", 10), "hello");
        assert_eq!(tail("hello", 3), "llo");
        assert_eq!(tail("hello", 0), "");
        // "é" is two bytes: a cut inside it moves forward past it.
        assert_eq!(tail("aéb", 2), "b");
        assert_eq!(tail("aéb", 3), "éb");
    }

    #[test]
    fn progress_keeps_only_the_most_recent_bytes() {
        let progress = Mutex::new(String::new());
        let chunk = "x".repeat(8192);
        for _ in 0..20 {
            append_progress(&progress, &chunk);
        }
        append_progress(&progress, "END");
        let buf = progress.lock().unwrap();
        assert_eq!(buf.len(), PROGRESS_MAX_BYTES);
        assert!(buf.ends_with("xEND"));

        drop(buf);
        append_progress(&progress, &"é".repeat(PROGRESS_MAX_BYTES));
        let buf = progress.lock().unwrap();
        assert!(buf.len() <= PROGRESS_MAX_BYTES && buf.chars().all(|c| c == 'é'));
    }
}
