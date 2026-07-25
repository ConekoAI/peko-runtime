//! Daemon-global registry of per-session inboxes and run permits.
//!
//! The registry is keyed by `session_id` (UUID) and is the single
//! source of truth for two things:
//!
//! 1. **Inbox**: the [`SessionInbox`] that holds queued steering
//!    messages and async task completions for the session. The IPC
//!    server pushes steering items here, the `AsyncExecutor` pushes
//!    completions here, and the in-flight `AgenticLoop` drains from
//!    here at the top of every iteration.
//! 2. **Run permit**: a [`Semaphore`] with one permit per session. A
//!    permit is held by the spawn future of an in-flight
//!    `AgenticLoop` for the duration of the run, and released when
//!    the spawn future returns. This is how the registry knows "is a
//!    run currently in-flight for this session?"
//!
//! Both the inbox and the semaphore are lazily created on first
//! access. There is no explicit cleanup: inboxes and permits live as
//! long as the daemon, and the map size is bounded by the number of
//! distinct sessions ever touched.

use std::collections::HashMap;
use std::sync::Arc;

use peko_extension_api::AsyncInboxLike;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// Factory that creates a fresh inbox on first access. The factory
/// runs only when a new session id is registered, so the inbox
/// implementation lives outside `peko-session` (in
/// `peko-extension-host`) and is injected via the factory closure.
/// This keeps `peko-session` from depending on `peko-extension-host`
/// (forbidden by the workspace dep-graph rules).
pub type InboxFactory = Arc<dyn Fn() -> Arc<dyn AsyncInboxLike> + Send + Sync + 'static>;

/// Per-session state held by the [`InboxRegistry`]. The inbox is
/// shared by the IPC server, the executor, and the loop. The
/// semaphore has a single permit; while it is held, a run is in
/// flight for the session.
struct InboxEntry {
    inbox: Arc<dyn AsyncInboxLike>,
    run_permit: Arc<Semaphore>,
}

impl InboxEntry {
    fn new(factory: &InboxFactory) -> Self {
        Self {
            inbox: factory(),
            run_permit: Arc::new(Semaphore::new(1)),
        }
    }
}

/// RAII guard for the per-session run permit. Holding one of these
/// means a run is in flight for the corresponding session. The
/// permit is released when the guard is dropped (typically when the
/// `AgenticLoop`'s spawn future returns).
pub struct RunPermitGuard {
    _permit: OwnedSemaphorePermit,
}

impl RunPermitGuard {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        Self { _permit: permit }
    }
}

/// Daemon-global registry of per-session inboxes and run permits.
///
/// All entry points are `async` because the inner map is protected by
/// a [`tokio::sync::Mutex`]. Lock hold times are short (just
/// HashMap access); the semaphore acquisition that follows is not
/// held under the map lock.
pub struct InboxRegistry {
    inner: Arc<Mutex<HashMap<String, InboxEntry>>>,
    factory: InboxFactory,
}

impl InboxRegistry {
    /// Construct a registry whose per-session inboxes are created
    /// by the given factory. Callers normally pass
    /// `Arc::new(|| Arc::new(peko_extension_host::SessionInbox::new()))`
    /// from root (or a test fake). The registry itself stays free
    /// of any dependency on the host crate.
    #[must_use]
    pub fn new(factory: InboxFactory) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            factory,
        }
    }

    /// Look up the inbox for `session_id`, creating a fresh entry
    /// (with an empty inbox and a run-permit semaphore initialized
    /// to 1) on first access. Idempotent: repeated calls with the
    /// same `session_id` return the same `Arc<dyn AsyncInboxLike>`.
    pub async fn get_or_create(&self, session_id: &str) -> Arc<dyn AsyncInboxLike> {
        let mut guard = self.inner.lock().await;
        let factory = Arc::clone(&self.factory);
        let entry = guard
            .entry(session_id.to_string())
            .or_insert_with(|| InboxEntry::new(&factory));
        Arc::clone(&entry.inbox)
    }

    /// Try to acquire the run permit for `session_id`. Returns
    /// `Some(RunPermitGuard)` if no run is currently in flight for
    /// the session, in which case the caller is responsible for
    /// starting one. Returns `None` if a run is already in flight;
    /// the caller should just enqueue their work (e.g. push a
    /// steering message to the inbox) and return.
    ///
    /// Lazy-creates the entry if it does not exist.
    pub async fn try_acquire_run(&self, session_id: &str) -> Option<RunPermitGuard> {
        // Step 1: clone the semaphore under the map lock so the
        // entry can't be removed (or its semaphore swapped) between
        // the lookup and the acquire.
        let semaphore = {
            let mut guard = self.inner.lock().await;
            let factory = Arc::clone(&self.factory);
            let entry = guard
                .entry(session_id.to_string())
                .or_insert_with(|| InboxEntry::new(&factory));
            Arc::clone(&entry.run_permit)
        };

        // Step 2: try to acquire without holding the map lock.
        semaphore.try_acquire_owned().ok().map(RunPermitGuard::new)
    }

    /// Best-effort snapshot: returns `true` if a run permit is
    /// currently held for `session_id`, `false` otherwise (including
    /// when the session has no entry at all).
    ///
    /// This is a snapshot; the state can change immediately after
    /// the call returns. Useful for telemetry, status displays, and
    /// tests — not for any correctness-critical gating.
    pub async fn peek_run_held(&self, session_id: &str) -> bool {
        let guard = self.inner.lock().await;
        guard
            .get(session_id)
            .is_some_and(|entry| entry.run_permit.available_permits() == 0)
    }

    /// Read-only access to the inbox for `session_id`. Returns
    /// `None` if no entry exists yet (caller can choose to lazily
    /// create via [`Self::get_or_create`] or treat the session as
    /// empty). Does NOT lazy-create, so the read path is
    /// non-mutating.
    pub async fn peek_inbox(&self, session_id: &str) -> Option<Arc<dyn AsyncInboxLike>> {
        let guard = self.inner.lock().await;
        guard.get(session_id).map(|entry| Arc::clone(&entry.inbox))
    }

    /// Number of sessions with a registered entry. For tests /
    /// metrics.
    pub async fn len(&self) -> usize {
        let guard = self.inner.lock().await;
        guard.len()
    }

    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.lock().await;
        guard.is_empty()
    }
}

impl Default for InboxRegistry {
    fn default() -> Self {
        // The default factory only works for callers that don't need
        // a real inbox — use [`InboxRegistry::new`] with a proper
        // factory for production code paths.
        Self::new(Arc::new(|| -> Arc<dyn AsyncInboxLike> {
            panic!(
                "InboxRegistry::default() factory called; \
                 use InboxRegistry::new(factory) instead"
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::*;
    use async_trait::async_trait;
    use peko_extension_api::{AsyncInboxItem, AsyncInboxLike, AsyncTaskStatus};
    use peko_tools_core::ToolResult;
    use serde_json::json;
    use std::path::PathBuf;
    use tokio::sync::Mutex as AsyncMutex;

    /// Minimal AsyncInboxLike stub for tests — does not exercise any
    /// envelope-conversion path (that's covered by peko-extension-host
    /// unit tests); just verifies the registry's HashMap + semaphore
    /// mechanics.
    #[derive(Debug, Default)]
    struct TestInbox {
        items: AsyncMutex<Vec<AsyncInboxItem>>,
    }

    #[async_trait]
    impl AsyncInboxLike for TestInbox {
        async fn drain_all(&self) -> Vec<AsyncInboxItem> {
            let mut g = self.items.lock().await;
            std::mem::take(&mut *g)
        }
    }

    fn test_factory() -> InboxFactory {
        Arc::new(|| Arc::new(TestInbox::default()) as Arc<dyn AsyncInboxLike>)
    }

    fn make_event(task_id: &str, session: &str) -> AsyncInboxItem {
        AsyncInboxItem::Completion(peko_extension_api::CompletionEnvelope {
            task_id: task_id.to_string(),
            tool_name: "shell".to_string(),
            result: json!({"exit_code": 0}),
            status: AsyncTaskStatus::Completed {
                result: ToolResult::success(json!({"exit_code": 0})),
            },
            completed_at: chrono::Utc::now(),
            output_path: PathBuf::from("/tmp/fake.ndjson"),
            parent_session_key: session.to_string(),
        })
    }

    #[tokio::test]
    async fn test_get_or_create_is_idempotent() {
        let reg = InboxRegistry::new(test_factory());
        let a = reg.get_or_create("s1").await;
        let b = reg.get_or_create("s1").await;
        assert!(Arc::ptr_eq(&a, &b), "same id must return the same Arc");
        let c = reg.get_or_create("s2").await;
        assert!(
            !Arc::ptr_eq(&a, &c),
            "different ids must return different Arcs"
        );
    }

    #[tokio::test]
    async fn test_try_acquire_run_succeeds_when_idle() {
        let reg = InboxRegistry::new(test_factory());
        // Trigger lazy creation by first calling get_or_create.
        let _inbox = reg.get_or_create("s1").await;
        let permit = reg.try_acquire_run("s1").await;
        assert!(permit.is_some(), "permit must be available when idle");
    }

    #[tokio::test]
    async fn test_try_acquire_run_fails_when_held() {
        let reg = InboxRegistry::new(test_factory());
        let _inbox = reg.get_or_create("s1").await;
        let first = reg.try_acquire_run("s1").await;
        assert!(first.is_some());
        let second = reg.try_acquire_run("s1").await;
        assert!(
            second.is_none(),
            "second acquire while first is held must return None"
        );
    }

    #[tokio::test]
    async fn test_try_acquire_run_succeeds_again_after_drop() {
        let reg = InboxRegistry::new(test_factory());
        let _inbox = reg.get_or_create("s1").await;
        {
            let first = reg.try_acquire_run("s1").await;
            assert!(first.is_some());
            // first drops at end of this scope
        }
        let second = reg.try_acquire_run("s1").await;
        assert!(
            second.is_some(),
            "permit must be re-acquirable after the first guard drops"
        );
    }

    #[tokio::test]
    async fn test_permits_are_per_session() {
        let reg = InboxRegistry::new(test_factory());
        let _inbox1 = reg.get_or_create("s1").await;
        let _inbox2 = reg.get_or_create("s2").await;
        let permit1 = reg.try_acquire_run("s1").await;
        assert!(permit1.is_some());
        // Holding a permit on s1 must not block acquiring a permit
        // on s2.
        let permit2 = reg.try_acquire_run("s2").await;
        assert!(permit2.is_some(), "permits are per-session, not global");
    }

    #[tokio::test]
    async fn test_try_acquire_run_lazy_creates_entry() {
        let reg = InboxRegistry::new(test_factory());
        // No prior get_or_create; try_acquire_run must still work
        // and return Some.
        let permit = reg.try_acquire_run("lazy").await;
        assert!(permit.is_some());
        assert_eq!(reg.len().await, 1);
    }

    #[tokio::test]
    async fn test_peek_run_held_reflects_state() {
        let reg = InboxRegistry::new(test_factory());
        let _inbox = reg.get_or_create("s1").await;
        assert!(!reg.peek_run_held("s1").await, "no permit held initially");
        let permit = reg.try_acquire_run("s1").await;
        assert!(reg.peek_run_held("s1").await, "permit held after acquire");
        drop(permit);
        assert!(!reg.peek_run_held("s1").await, "permit released after drop");
        // Unknown session -> false.
        assert!(!reg.peek_run_held("never-touched").await);
    }

    #[tokio::test]
    async fn test_inbox_factory_returns_drainable_inbox() {
        // Smoke: the factory closure produces an inbox whose items
        // come back through drain_all(). The test stub starts empty,
        // so we just verify the trait surface works end-to-end.
        let reg = InboxRegistry::new(test_factory());
        let inbox = reg.get_or_create("s1").await;
        let items = inbox.drain_all().await;
        assert!(items.is_empty());
    }
}
