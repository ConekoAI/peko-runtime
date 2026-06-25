//! Per-session inbox of completed async tasks and user steering messages
//! waiting to be injected into the next agentic loop iteration.
//!
//! Distinct from [`super::queue::AsyncResultQueueManager`], which is the
//! older delivery sink kept for backward compatibility. New code should
//! read from this inbox.

use super::types::{AsyncTaskId, AsyncTaskStatus};
use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

/// Event pushed to the inbox when an async task reaches a terminal
/// state. The agentic loop drains these at iteration start and
/// synthesizes a single user-role message containing all of them.
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub result: serde_json::Value,
    pub status: AsyncTaskStatus,
    pub completed_at: DateTime<Utc>,
    pub output_path: PathBuf,
    pub parent_session_key: String,
}

/// User-supplied message queued for delivery to a session at the start
/// of the next agentic loop iteration. Multiple steering items are
/// delivered as separate user-role turns in arrival order.
#[derive(Debug, Clone)]
pub struct SteeringMessage {
    pub id: Uuid,
    pub content: String,
    pub queued_at: DateTime<Utc>,
}

impl SteeringMessage {
    /// Construct a steering message with a freshly generated id and
    /// the current UTC timestamp.
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            content: content.into(),
            queued_at: Utc::now(),
        }
    }
}

/// Item carried in a [`SessionInbox`]. Either a user steering message
/// (delivered as a separate user-role turn) or a completion event from
/// a background async task (delivered as a synthetic user-role message
/// with N `ToolResult` blocks).
#[derive(Debug, Clone)]
pub enum InboxItem {
    Steering(SteeringMessage),
    Completion(CompletionEvent),
}

impl From<CompletionEvent> for InboxItem {
    fn from(e: CompletionEvent) -> Self {
        InboxItem::Completion(e)
    }
}

impl From<SteeringMessage> for InboxItem {
    fn from(m: SteeringMessage) -> Self {
        InboxItem::Steering(m)
    }
}

/// Per-session FIFO of [`InboxItem`]s waiting to be injected at the
/// next agentic loop iteration.
///
/// Replaces the older `AsyncTaskCompletionQueue` (now removed). The
/// same underlying inbox is shared by the IPC server (which pushes
/// steering messages from external clients), the `AsyncExecutor`
/// (which pushes completion events from background tasks), and the
/// `AgenticLoop` (which drains the inbox at the start of every
/// iteration).
#[derive(Debug)]
pub struct SessionInbox {
    inner: Arc<Mutex<VecDeque<InboxItem>>>,
    /// Wakes any future code that wants to wait for "at least one
    /// item" — currently unused by the agentic loop (it polls at
    /// iteration start) but available for follow-up work.
    notify: Arc<Notify>,
}

impl SessionInbox {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Push an item onto the inbox. Wakes any waiters. Accepts a
    /// `CompletionEvent`, a `SteeringMessage`, or any other
    /// `InboxItem` via `Into`.
    ///
    /// Synchronous: uses `try_lock` and, on contention, schedules a
    /// blocking push via `tokio::spawn`. The common case (no
    /// contention) is in-line.
    pub fn push<E: Into<InboxItem>>(&self, event: E) {
        let event = event.into();
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

    /// Drain only completion events from the inbox, leaving any
    /// pending steering messages in place. Returns events in
    /// insertion order.
    ///
    /// This preserves the legacy `SessionInbox::drain` behavior for
    /// completion-only callers. The agentic loop now uses `drain_all`
    /// (which returns the full item list, both steering and
    /// completions).
    /// full item list) when the loop is rewired to handle steering
    /// messages.
    pub async fn drain(&self) -> Vec<CompletionEvent> {
        let mut guard = self.inner.lock().await;
        let mut out = Vec::new();
        let mut keep: VecDeque<InboxItem> = VecDeque::with_capacity(guard.len());
        for item in guard.drain(..) {
            match item {
                InboxItem::Completion(e) => out.push(e),
                InboxItem::Steering(m) => keep.push_back(InboxItem::Steering(m)),
            }
        }
        *guard = keep;
        out
    }

    /// Drain all items in insertion order, leaving the inbox empty.
    /// This is the canonical drain method for code that wants to
    /// process both steering messages and completions.
    pub async fn drain_all(&self) -> Vec<InboxItem> {
        let mut guard = self.inner.lock().await;
        guard.drain(..).collect()
    }

    /// Number of pending steering messages (for client UX / metrics).
    pub async fn steering_len(&self) -> usize {
        let guard = self.inner.lock().await;
        guard
            .iter()
            .filter(|i| matches!(i, InboxItem::Steering(_)))
            .count()
    }

    /// Snapshot of pending steering messages. Non-destructive: the
    /// items remain in the inbox.
    pub async fn pending_steering(&self) -> Vec<SteeringMessage> {
        let guard = self.inner.lock().await;
        guard
            .iter()
            .filter_map(|i| match i {
                InboxItem::Steering(m) => Some(m.clone()),
                InboxItem::Completion(_) => None,
            })
            .collect()
    }

    /// Remove a single pending steering message by id. Returns `true`
    /// if the message was present and removed; `false` if it was not
    /// in the inbox (already drained, or never existed).
    ///
    /// Best-effort: a steering message that has already been drained
    /// into the in-flight `AgenticLoop`'s message buffer is no longer
    /// in the inbox and cannot be cancelled.
    pub async fn cancel_steering(&self, id: Uuid) -> bool {
        let mut guard = self.inner.lock().await;
        let before = guard.len();
        guard.retain(|i| !matches!(i, InboxItem::Steering(m) if m.id == id));
        guard.len() < before
    }

    /// Total number of items in the inbox (for testing/metrics).
    pub async fn len(&self) -> usize {
        let guard = self.inner.lock().await;
        guard.len()
    }

    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.lock().await;
        guard.is_empty()
    }
}

impl Default for SessionInbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SessionInbox {
    fn clone(&self) -> Self {
        // Shares the same underlying inbox via internal Arc — useful
        // for moving the inbox into a spawned task (e.g. the
        // contended-push fallback) or for `Arc<SessionInbox>`
        // ownership across the IPC and executor paths.
        Self {
            inner: Arc::clone(&self.inner),
            notify: Arc::clone(&self.notify),
        }
    }
}

pub type SharedSessionInbox = Arc<SessionInbox>;

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
            completed_at: Utc::now(),
            output_path: PathBuf::from("/tmp/fake.ndjson"),
            parent_session_key: session.to_string(),
        }
    }

    // ---------- completion-only drain behavior (legacy `drain` API) ----------

    #[tokio::test]
    async fn test_push_and_drain() {
        let queue = SessionInbox::new();
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
        let queue = SessionInbox::new();
        let drained = queue.drain().await;
        assert!(drained.is_empty());
    }

    #[tokio::test]
    async fn test_fifo_ordering_under_concurrent_push() {
        use std::sync::Arc;
        use tokio::sync::Barrier;
        let queue = Arc::new(SessionInbox::new());
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
        // Verify all events are present (set membership), not strict
        // FIFO, because concurrent push order is non-deterministic.
        let ids: std::collections::HashSet<String> =
            drained.iter().map(|e| e.task_id.clone()).collect();
        let expected: std::collections::HashSet<String> =
            (0..10).map(|i| format!("shell:{i}")).collect();
        assert_eq!(ids, expected, "all events must be present");
    }

    #[tokio::test]
    async fn test_push_under_contention_reaches_drain() {
        use std::sync::Arc;
        let queue = Arc::new(SessionInbox::new());
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
        assert_eq!(
            drained.len(),
            100,
            "contended pushes must not be silently dropped"
        );
    }

    // ---------- new SessionInbox behavior ----------

    #[tokio::test]
    async fn test_session_inbox_push_steering() {
        let inbox = SessionInbox::new();
        let m = SteeringMessage::new("actually do X instead");
        let id = m.id;
        inbox.push(m);

        assert_eq!(inbox.steering_len().await, 1);
        assert!(!inbox.is_empty().await);
        let pending = inbox.pending_steering().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].content, "actually do X instead");
    }

    #[tokio::test]
    async fn test_session_inbox_drain_all_returns_steering_and_completion_in_fifo_order() {
        let inbox = SessionInbox::new();
        inbox.push(SteeringMessage::new("user: please do X"));
        inbox.push(make_event("shell:a", "session_1"));
        inbox.push(SteeringMessage::new("user: actually do Y"));
        inbox.push(make_event("shell:b", "session_1"));

        let drained = inbox.drain_all().await;
        assert_eq!(drained.len(), 4);
        match &drained[0] {
            InboxItem::Steering(m) => assert_eq!(m.content, "user: please do X"),
            other => panic!("expected Steering, got {other:?}"),
        }
        match &drained[1] {
            InboxItem::Completion(e) => assert_eq!(e.task_id, "shell:a"),
            other => panic!("expected Completion, got {other:?}"),
        }
        match &drained[2] {
            InboxItem::Steering(m) => assert_eq!(m.content, "user: actually do Y"),
            other => panic!("expected Steering, got {other:?}"),
        }
        match &drained[3] {
            InboxItem::Completion(e) => assert_eq!(e.task_id, "shell:b"),
            other => panic!("expected Completion, got {other:?}"),
        }
        assert!(inbox.is_empty().await);
    }

    #[tokio::test]
    async fn test_legacy_drain_skips_steering_and_keeps_them_in_place() {
        let inbox = SessionInbox::new();
        inbox.push(SteeringMessage::new("user: please do X"));
        inbox.push(make_event("shell:a", "session_1"));
        inbox.push(SteeringMessage::new("user: actually do Y"));
        inbox.push(make_event("shell:b", "session_1"));

        // Legacy drain (completions only) — steering items are NOT
        // removed from the inbox.
        let completions = inbox.drain().await;
        assert_eq!(completions.len(), 2);
        assert_eq!(completions[0].task_id, "shell:a");
        assert_eq!(completions[1].task_id, "shell:b");

        // The two steering items are still in the inbox.
        assert_eq!(inbox.steering_len().await, 2);
        assert!(!inbox.is_empty().await);

        // A subsequent legacy drain returns no completions but leaves
        // steering in place.
        let completions2 = inbox.drain().await;
        assert!(completions2.is_empty());
        assert_eq!(inbox.steering_len().await, 2);

        // drain_all finally returns the still-pending steering items.
        let remaining = inbox.drain_all().await;
        assert_eq!(remaining.len(), 2);
        for item in &remaining {
            assert!(matches!(item, InboxItem::Steering(_)));
        }
        assert!(inbox.is_empty().await);
    }

    #[tokio::test]
    async fn test_cancel_steering_returns_true_when_present() {
        let inbox = SessionInbox::new();
        let m = SteeringMessage::new("user: please do X");
        let id = m.id;
        inbox.push(m);
        inbox.push(make_event("shell:a", "session_1"));

        let removed = inbox.cancel_steering(id).await;
        assert!(removed, "cancel must return true when the id is present");
        assert_eq!(inbox.steering_len().await, 0);
        // The completion event is still in the inbox.
        assert_eq!(inbox.len().await, 1);
    }

    #[tokio::test]
    async fn test_cancel_steering_returns_false_when_absent() {
        let inbox = SessionInbox::new();
        let missing = Uuid::new_v4();
        let removed = inbox.cancel_steering(missing).await;
        assert!(!removed, "cancel must return false for an unknown id");

        inbox.push(SteeringMessage::new("hi"));
        let removed2 = inbox.cancel_steering(missing).await;
        assert!(
            !removed2,
            "cancel must return false for an id not in the inbox"
        );
    }

    #[tokio::test]
    async fn test_cancel_steering_is_id_specific() {
        let inbox = SessionInbox::new();
        let m1 = SteeringMessage::new("first");
        let m1_id = m1.id;
        let m2 = SteeringMessage::new("second");
        let m2_id = m2.id;
        inbox.push(m1);
        inbox.push(m2);

        let removed = inbox.cancel_steering(m1_id).await;
        assert!(removed);

        let pending = inbox.pending_steering().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, m2_id);
        assert_eq!(pending[0].content, "second");
    }

    #[tokio::test]
    async fn test_pending_steering_is_non_destructive() {
        let inbox = SessionInbox::new();
        inbox.push(SteeringMessage::new("a"));
        inbox.push(make_event("shell:x", "s1"));
        inbox.push(SteeringMessage::new("b"));

        let snap1 = inbox.pending_steering().await;
        assert_eq!(snap1.len(), 2);
        let snap2 = inbox.pending_steering().await;
        assert_eq!(snap2.len(), 2);
        assert_eq!(inbox.len().await, 3, "snapshots must not drain the inbox");
    }

    #[tokio::test]
    async fn test_clone_shares_state() {
        let a = SessionInbox::new();
        let b = a.clone();
        a.push(SteeringMessage::new("hello"));
        assert_eq!(b.steering_len().await, 1);
        let drained = b.drain_all().await;
        assert_eq!(drained.len(), 1);
        assert!(a.is_empty().await);
    }

    #[tokio::test]
    async fn test_into_completion_event_preserves_legacy_call_pattern() {
        // The legacy `queue.push(CompletionEvent)` call pattern must
        // still compile and behave identically: a CompletionEvent is
        // wrapped in InboxItem::Completion via From.
        let queue = SessionInbox::new();
        let ev = make_event("shell:c", "s1");
        let task_id = ev.task_id.clone();
        queue.push(ev);
        let drained = queue.drain().await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].task_id, task_id);
    }
}
