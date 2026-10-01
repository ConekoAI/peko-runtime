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

    #[test]
    fn notify_without_handler_is_noop() {
        uninstall_completion_wake_handler();
        notify_completion_wake(notice("s1")); // must not panic
    }

    #[test]
    fn installed_handler_receives_notice() {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let hits_w = Arc::clone(&hits);
        install_completion_wake_handler(Arc::new(move |n| {
            hits_w.lock().unwrap().push(n.session_key);
        }));
        notify_completion_wake(notice("s-wake"));
        assert_eq!(hits.lock().unwrap().as_slice(), &["s-wake".to_string()]);
        uninstall_completion_wake_handler();
    }

    #[test]
    fn reinstall_replaces_previous_handler() {
        let first = Arc::new(Mutex::new(0usize));
        let second = Arc::new(Mutex::new(0usize));
        let first_w = Arc::clone(&first);
        install_completion_wake_handler(Arc::new(move |_| {
            *first_w.lock().unwrap() += 1;
        }));
        let second_w = Arc::clone(&second);
        install_completion_wake_handler(Arc::new(move |_| {
            *second_w.lock().unwrap() += 1;
        }));
        notify_completion_wake(notice("s"));
        assert_eq!(*first.lock().unwrap(), 0, "replaced handler must not fire");
        assert_eq!(*second.lock().unwrap(), 1);
        uninstall_completion_wake_handler();
    }
}
