//! Completion-driven wake hook — the "background task wakes an idle
//! agent" half of async delivery.
//!
//! ## The gap this closes
//!
//! A terminal async task pushes a `CompletionEvent` (or, for cron
//! spawns, a `SteeringMessage`) into the parent session's inbox. While
//! a run is in flight that is sufficient: the agentic loop drains the
//! inbox at the top of the next iteration. But when the session is
//! **idle** — the common case for a genuinely background task — nobody
//! consumed the push: the event waited for the next unrelated user
//! message, and the post-run steering drains used to delete it
//! outright.
//!
//! The executor cannot drive a turn itself (it lives in the framework
//! layer, below the principal/session machinery), so instead it fires
//! a process-global hook installed by the daemon at startup
//! ([`install_completion_wake_handler`]). The daemon-side handler
//! re-acquires the session's run permit (`InboxRegistry::try_acquire_run`
//! — returning `None` means a run is in flight and will drain the
//! inbox itself), then drives a successor turn whose first iteration
//! drains the inbox naturally.
//!
//! The hook fires only for deliveries the executor actually made
//! (`deliver_completion = true`); router-dispatched calls that
//! completed synchronously inside the router timeout never push, so
//! they never wake.

use std::sync::{Arc, OnceLock, RwLock};

/// Turn input for a wake/successor turn whose real payload is the
/// session inbox's queued items — the successor run's first-iteration
/// drain injects them (as a synthetic completion message or steering
/// text), so this marker only needs to explain WHY the turn exists in
/// the persisted transcript. Shared by the daemon's completion-wake
/// handler and the post-run successor chains so the wording stays
/// consistent.
pub const WAKE_TURN_MARKER: &str = "[peko] Background work completed while no turn was running; \
     the queued result is injected with this turn. Act on it as appropriate.";

/// One terminal delivery that may need to wake an idle session.
#[derive(Debug, Clone)]
pub struct CompletionWakeNotice {
    /// Inbox key the delivery landed in — the session to drive.
    pub session_key: String,
    /// The task that terminated.
    pub task_id: String,
    /// Tool the task ran (for logging / message formatting).
    pub tool_name: String,
    /// Owning principal (string form) stamped on the task, if any.
    /// The daemon handler resolves the principal from this; `None`
    /// means the task is unattributed and cannot be routed to a
    /// principal — the handler logs and leaves the event queued.
    pub principal_id: Option<String>,
    /// `true` when the delivery was a `SteeringMessage` (cron's
    /// `principal_root_session_key` branch) rather than a
    /// `CompletionEvent`.
    pub via_steering: bool,
}

/// Process-global wake handler. Must be cheap and non-blocking — the
/// daemon implementation `tokio::spawn`s the actual turn driving.
pub type CompletionWakeHandler = Arc<dyn Fn(CompletionWakeNotice) + Send + Sync>;

static WAKE_HANDLER: OnceLock<RwLock<Option<CompletionWakeHandler>>> = OnceLock::new();

fn slot() -> &'static RwLock<Option<CompletionWakeHandler>> {
    WAKE_HANDLER.get_or_init(|| RwLock::new(None))
}

/// Install (or replace) the process-global completion wake handler.
/// Called by the daemon once the shared `InboxRegistry` +
/// `PrincipalManager` exist. Replacing is allowed so tests and daemon
/// restarts within one process do not stack handlers.
pub fn install_completion_wake_handler(handler: CompletionWakeHandler) {
    let mut guard = slot().write().unwrap_or_else(|e| e.into_inner());
    *guard = Some(handler);
}

/// Remove the handler (test teardown / daemon shutdown).
pub fn uninstall_completion_wake_handler() {
    let mut guard = slot().write().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

/// Fire the wake hook, if one is installed. Called by
/// `AsyncExecutor::execute_inner` immediately after the terminal
/// inbox push. A missing handler (CLI one-shots, tests) is a no-op —
/// the completion still waits in the inbox for the next run.
pub fn notify_completion_wake(notice: CompletionWakeNotice) {
    let handler = {
        let guard = slot().read().unwrap_or_else(|e| e.into_inner());
        guard.clone()
    };
    if let Some(handler) = handler {
        handler(notice);
    }
}

#[cfg(test)]
mod tests {
    //! The hook is process-global and `cargo test` runs tests in parallel:
    //! every test that installs or fires it is `serial(wake_hook)` (shared
    //! with the executor's wake tests), and assertions only count notices
    //! for this test's own session keys, since unrelated executor tests
    //! may complete tasks while a handler is installed.
    use super::*;
    use std::sync::Mutex;

    fn notice(session: &str) -> CompletionWakeNotice {
        CompletionWakeNotice {
            session_key: session.to_string(),
            task_id: "tool:t1".to_string(),
            tool_name: "tool".to_string(),
            principal_id: None,
            via_steering: false,
        }
    }

    /// Install a handler recording session keys that start with `prefix`.
    fn record(prefix: &'static str) -> Arc<Mutex<Vec<String>>> {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let hits_w = Arc::clone(&hits);
        install_completion_wake_handler(Arc::new(move |n| {
            if n.session_key.starts_with(prefix) {
                hits_w.lock().unwrap().push(n.session_key);
            }
        }));
        hits
    }

    #[test]
    #[serial_test::serial(wake_hook)]
    fn notify_without_handler_is_noop() {
        uninstall_completion_wake_handler();
        notify_completion_wake(notice("wake-noop")); // must not panic
    }

    #[test]
    #[serial_test::serial(wake_hook)]
    fn installed_handler_receives_notice() {
        let hits = record("wake-installed");
        notify_completion_wake(notice("wake-installed"));
        uninstall_completion_wake_handler();
        assert_eq!(
            hits.lock().unwrap().as_slice(),
            &["wake-installed".to_string()]
        );
    }

    #[test]
    #[serial_test::serial(wake_hook)]
    fn reinstall_replaces_previous_handler() {
        let first = record("wake-reinstall");
        let second = record("wake-reinstall");
        notify_completion_wake(notice("wake-reinstall"));
        uninstall_completion_wake_handler();
        assert!(
            first.lock().unwrap().is_empty(),
            "replaced handler must not fire"
        );
        assert_eq!(second.lock().unwrap().len(), 1);
    }
}
