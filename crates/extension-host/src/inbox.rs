//! Per-session inbox and the cross-boundary sink traits.
//!
//! ## Why traits?
//!
//! Before Phase 8, `AsyncExecutor` held a concrete
//! `Arc<crate::session::InboxRegistry>`. After moving the executor
//! into `peko-extension-host`, the host crate would have had to
//! depend on root `session::*` — a forbidden direction (the host is
//! a leaf; the root is the facade).
//!
//! Instead, the host defines:
//!
//! - [`SessionInboxSink`] — the minimum surface `AsyncExecutor`
//!   needs: push an [`InboxItem`] synchronously. `SessionInbox`
//!   implements this trait.
//! - [`InboxSinkProvider`] — keyed lookup of an
//!   `Arc<dyn SessionInboxSink>`. The host ships a default
//!   [`InboxSinkRegistry`] implementation; the root's richer
//!   `crate::session::InboxRegistry` also implements this trait so
//!   the daemon can wire the executor against its session-scoped
//!   state without any cycle.
//!
//! The root's `InboxRegistry` keeps its `try_acquire_run` /
//! `peek_run_held` API for daemon-side run-permit bookkeeping; only
//! the `get_or_create`-style lookup is funnelled through
//! `InboxSinkProvider`.

use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

/// Event pushed to the inbox when an async task reaches a terminal
/// state. The agentic loop drains these at iteration start and
/// synthesizes a single user-role message containing all of them.
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub task_id: String,
    pub tool_name: String,
    pub result: serde_json::Value,
    pub status: peko_extension_api::AsyncTaskStatus,
    pub completed_at: DateTime<Utc>,
    pub output_path: std::path::PathBuf,
    pub parent_session_key: String,
}

/// User-supplied message queued for delivery to a session at the
/// start of the next agentic loop iteration.
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

/// Item carried in a [`SessionInboxSink`]. Either a user steering
/// message or a completion event from a background async task.
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

/// Minimum surface `AsyncExecutor` needs from a per-session inbox.
///
/// `AsyncExecutor::execute_inner` pushes a completion event (or a
/// steering message) into the inbox from a spawned task. Pushing is
/// synchronous — the underlying type may spawn a fallback task to
/// acquire the lock, but the trait call itself does not need to be
/// `async`.
///
/// Implementors must be `Send + Sync` so the executor can hold
/// `Arc<dyn SessionInboxSink>` across `.await` points.
pub trait SessionInboxSink: Send + Sync + 'static {
    /// Push an item into the inbox.
    fn push(&self, item: InboxItem);
}

/// Provider of per-session inboxes keyed by an opaque session id.
///
/// The executor's `InboxSinkProvider` is normally the daemon's
/// shared `crate::session::InboxRegistry` (root crate), but the
/// host also ships [`InboxSinkRegistry`] for standalone / test use.
#[async_trait::async_trait]
pub trait InboxSinkProvider: Send + Sync + 'static {
    /// Look up (or create) the inbox for `session_key` and return a
    /// clonable sink. Two calls with the same key must return sinks
    /// that share underlying state.
    async fn get_or_create_sink(&self, session_key: &str) -> Arc<dyn SessionInboxSink>;
}

// =============================================================================
// Concrete session inbox — used by both `InboxSinkRegistry` and (via trait impl)
// by the root's `crate::session::InboxRegistry`.
// =============================================================================

/// Per-session FIFO of [`InboxItem`]s waiting to be injected at the
/// next agentic loop iteration.
///
/// Cloning shares the same underlying queue via the internal `Arc`.
/// The executor's spawned-task path calls `push` from non-async
/// contexts and uses `try_lock` for the common case (no contention),
/// falling back to a `tokio::spawn` blocking push otherwise.
#[derive(Debug)]
pub struct SessionInbox {
    inner: Arc<Mutex<VecDeque<InboxItem>>>,
    notify: Arc<Notify>,
}

impl Default for SessionInbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SessionInbox {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            notify: Arc::clone(&self.notify),
        }
    }
}

impl SessionInbox {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Synchronous push. Uses `try_lock`; on contention, schedules a
    /// blocking push via `tokio::spawn`.
    pub fn push<E: Into<InboxItem>>(&self, item: E) {
        let item = item.into();
        if let Ok(mut guard) = self.inner.try_lock() {
            guard.push_back(item);
            self.notify.notify_one();
        } else {
            let this = self.clone();
            tokio::spawn(async move {
                let mut guard = this.inner.lock().await;
                guard.push_back(item);
                this.notify.notify_one();
            });
        }
    }

    /// Drain all items in insertion order.
    pub async fn drain_all(&self) -> Vec<InboxItem> {
        let mut guard = self.inner.lock().await;
        guard.drain(..).collect()
    }

    /// Drain only completion events, leaving any pending steering
    /// messages in place.
    pub async fn drain_completions(&self) -> Vec<CompletionEvent> {
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

    /// Snapshot of pending steering messages.
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

    /// Number of pending steering messages (for client UX / metrics).
    pub async fn steering_len(&self) -> usize {
        let guard = self.inner.lock().await;
        guard
            .iter()
            .filter(|i| matches!(i, InboxItem::Steering(_)))
            .count()
    }

    /// Remove a single pending steering message by id. Returns `true`
    /// if it was present.
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

impl SessionInboxSink for SessionInbox {
    fn push(&self, item: InboxItem) {
        SessionInbox::push(self, item);
    }
}

/// Default [`InboxSinkProvider`] implementation. Owns a `HashMap` of
/// per-key inboxes and creates a fresh [`SessionInbox`] on first
/// lookup.
#[derive(Debug, Default)]
pub struct InboxSinkRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<SessionInbox>>>>,
}

impl Clone for InboxSinkRegistry {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl InboxSinkRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up or create the [`SessionInbox`] for `key`. Returns the
    /// concrete inbox type so callers (e.g. tests) can drain it
    /// without trait-object downcasting.
    pub async fn get_or_create(&self, key: &str) -> Arc<SessionInbox> {
        let mut guard = self.inner.lock().await;
        guard
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(SessionInbox::new()))
            .clone()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    /// Direct lookup (no create). Used by tests / observability.
    pub async fn peek(&self, key: &str) -> Option<Arc<SessionInbox>> {
        self.inner.lock().await.get(key).cloned()
    }
}

#[async_trait::async_trait]
impl InboxSinkProvider for InboxSinkRegistry {
    async fn get_or_create_sink(&self, session_key: &str) -> Arc<dyn SessionInboxSink> {
        let mut guard = self.inner.lock().await;
        guard
            .entry(session_key.to_string())
            .or_insert_with(|| Arc::new(SessionInbox::new()))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_event(task_id: &str, session: &str) -> CompletionEvent {
        CompletionEvent {
            task_id: task_id.to_string(),
            tool_name: "tool".to_string(),
            result: json!({"ok": true}),
            status: peko_extension_api::AsyncTaskStatus::Completed {
                result: peko_tools_core::ToolResult::success(json!({"ok": true})),
            },
            completed_at: Utc::now(),
            output_path: std::path::PathBuf::from("/tmp/out"),
            parent_session_key: session.to_string(),
        }
    }

    #[tokio::test]
    async fn session_inbox_push_drain() {
        let inbox = SessionInbox::new();
        inbox.push(SteeringMessage::new("hi"));
        inbox.push(make_event("t1", "s"));
        assert_eq!(inbox.len().await, 2);
        let items = inbox.drain_all().await;
        assert_eq!(items.len(), 2);
        assert!(inbox.is_empty().await);
    }

    #[tokio::test]
    async fn drain_completions_keeps_steering() {
        let inbox = SessionInbox::new();
        inbox.push(SteeringMessage::new("hello"));
        inbox.push(make_event("t1", "s"));
        inbox.push(make_event("t2", "s"));
        let completions = inbox.drain_completions().await;
        assert_eq!(completions.len(), 2);
        assert_eq!(inbox.steering_len().await, 1);
    }

    #[tokio::test]
    async fn cancel_steering() {
        let inbox = SessionInbox::new();
        let s = SteeringMessage::new("hi");
        let id = s.id;
        inbox.push(s);
        assert!(inbox.cancel_steering(id).await);
        assert!(!inbox.cancel_steering(id).await);
    }

    #[tokio::test]
    async fn sink_registry_creates_once() {
        let reg = InboxSinkRegistry::new();
        // First lookup creates; second lookup (via either the
        // inherent method or the trait method) returns the same
        // shared inbox.
        let a = reg.get_or_create("s1").await;
        let b = reg.get_or_create("s1").await;
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(reg.len().await, 1);
        // Sink lookup also returns an inbox backed by the same
        // underlying `SessionInbox` — we verify by checking that a
        // push through the sink lands in `a`'s drain.
        let sink = reg.get_or_create_sink("s1").await;
        sink.push(InboxItem::Steering(SteeringMessage::new("hi")));
        let items = a.drain_all().await;
        assert_eq!(items.len(), 1);
    }
}
