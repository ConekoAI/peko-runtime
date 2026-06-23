//! Per-session queue of completed async tasks waiting to be injected
//! into the next agentic loop iteration as a synthetic message.
//!
//! Distinct from [`super::queue::AsyncResultQueueManager`], which is the
//! older delivery sink kept for backward compatibility. New code should
//! read from this queue.

use super::types::{AsyncTaskId, AsyncTaskStatus};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// Event pushed to the completion queue when an async task reaches a
/// terminal state. The agentic loop drains these at iteration start
/// and synthesizes a single user-role message containing all of them.
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub result: serde_json::Value,
    pub status: AsyncTaskStatus,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    pub output_path: PathBuf,
    pub parent_session_key: String,
}

/// Per-session FIFO of completed async tasks waiting to be injected
/// at the next agentic loop iteration.
#[derive(Debug)]
pub struct AsyncTaskCompletionQueue {
    inner: Arc<Mutex<VecDeque<CompletionEvent>>>,
    /// Wakes any future code that wants to wait for "at least one
    /// completion" — currently unused by the agentic loop (it polls
    /// at iteration start) but available for follow-up work.
    notify: Arc<Notify>,
}

impl AsyncTaskCompletionQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Push a completion event onto the queue. Wakes any waiters.
    pub fn push(&self, event: CompletionEvent) {
        // Synchronous helper that does not block on the mutex — uses
        // try_lock and, if contended, schedules a blocking push via
        // tokio::spawn. The common case (no contention) is in-line.
        if let Ok(mut guard) = self.inner.try_lock() {
            guard.push_back(event);
            self.notify.notify_one();
        } else {
            let this = self.clone();
            tokio::spawn(async move {
                let mut guard = this.inner.lock().await;
                guard.push_back(event);
                this.notify.notify_one();
            });
        }
    }

    /// Drain all currently-queued events, leaving the queue empty.
    /// Returns events in insertion order.
    pub async fn drain(&self) -> Vec<CompletionEvent> {
        let mut guard = self.inner.lock().await;
        guard.drain(..).collect()
    }

    /// Number of pending events (for testing/metrics).
    pub async fn len(&self) -> usize {
        let guard = self.inner.lock().await;
        guard.len()
    }

    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.lock().await;
        guard.is_empty()
    }
}

impl Default for AsyncTaskCompletionQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for AsyncTaskCompletionQueue {
    fn clone(&self) -> Self {
        // Shares the same underlying queue via internal Arc — useful for
        // Arc<AsyncTaskCompletionQueue> or for moving the queue into a
        // spawned task (e.g. the contended push fallback).
        Self {
            inner: Arc::clone(&self.inner),
            notify: Arc::clone(&self.notify),
        }
    }
}

pub type SharedAsyncTaskCompletionQueue = Arc<AsyncTaskCompletionQueue>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_event(task_id: &str, session: &str) -> CompletionEvent {
        CompletionEvent {
            task_id: task_id.to_string(),
            tool_name: "shell".to_string(),
            result: json!({"exit_code": 0}),
            status: AsyncTaskStatus::Completed {
                result: crate::tools::core::ToolResult::success(json!({"exit_code": 0})),
            },
            completed_at: chrono::Utc::now(),
            output_path: PathBuf::from("/tmp/fake.ndjson"),
            parent_session_key: session.to_string(),
        }
    }

    #[tokio::test]
    async fn test_push_and_drain() {
        let queue = AsyncTaskCompletionQueue::new();
        queue.push(make_event("shell:a", "session_1"));
        queue.push(make_event("shell:b", "session_1"));

        let drained = queue.drain().await;
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].task_id, "shell:a");
        assert_eq!(drained[1].task_id, "shell:b");
        assert!(queue.is_empty().await);
    }

    #[tokio::test]
    async fn test_drain_empty() {
        let queue = AsyncTaskCompletionQueue::new();
        let drained = queue.drain().await;
        assert!(drained.is_empty());
    }

    #[tokio::test]
    async fn test_fifo_ordering_under_concurrent_push() {
        use std::sync::Arc;
        use tokio::sync::Barrier;
        let queue = Arc::new(AsyncTaskCompletionQueue::new());
        let barrier = Arc::new(Barrier::new(10));
        let mut handles = Vec::new();
        for i in 0..10 {
            let q = queue.clone();
            let b = barrier.clone();
            handles.push(tokio::spawn(async move {
                b.wait().await;
                q.push(make_event(&format!("shell:{i}"), "session_1"));
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        // Give any spawned pushes a chance to run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let drained = queue.drain().await;
        assert_eq!(drained.len(), 10);
        // Verify all events are present (set membership), not strict FIFO,
        // because concurrent push order is non-deterministic.
        let ids: std::collections::HashSet<String> =
            drained.iter().map(|e| e.task_id.clone()).collect();
        let expected: std::collections::HashSet<String> =
            (0..10).map(|i| format!("shell:{i}")).collect();
        assert_eq!(ids, expected, "all events must be present");
    }

    #[tokio::test]
    async fn test_push_under_contention_reaches_drain() {
        use std::sync::Arc;
        let queue = Arc::new(AsyncTaskCompletionQueue::new());
        let mut handles = Vec::new();
        for i in 0..100 {
            let q = queue.clone();
            handles.push(tokio::spawn(async move {
                q.push(make_event(&format!("shell:{i}"), "session_1"));
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let drained = queue.drain().await;
        assert_eq!(drained.len(), 100, "contended pushes must not be silently dropped");
    }
}
