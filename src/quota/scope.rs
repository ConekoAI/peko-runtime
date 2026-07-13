//! `QuotaScope` — task-local quota meter propagation (F19).
//!
//! F18 manually threaded `Arc<QuotaMeter>` through every struct along
//! the run path (Agent → AgenticLoop → SubagentExecutor → RootRouter →
//! PrincipalContext). That forced every LLM call site to remember to
//! `check()` and `charge()`.
//!
//! F19 replaces the per-struct plumbing with a single task-local:
//! callers open a [`QuotaScope::with`] at the run entrypoint, and
//! every [`MeteredProvider`](crate::providers::MeteredProvider) created
//! inside that scope automatically charges the right meter. New
//! LLM-call sites (MCP sampling, future RAG, future agent-to-agent
//! bridges) get attribution for free as long as the spawning code
//! opens a scope.
//!
//! ## Usage
//!
//! ```ignore
//! // Run entrypoint
//! QuotaScope::with(meter, async move {
//!     let provider = resolver.build(...).await?;
//!     let metered = MeteredProvider::from_current_scope(provider);
//!     metered.chat_with_tools(...).await  // auto-charges `meter`
//! }).await
//! ```
//!
//! ## Stacking (F20)
//!
//! Multiple meters can be active simultaneously by nesting `with`
//! calls. Each `with` appends to the active stack:
//!
//! ```ignore
//! QuotaScope::with(principal_meter, async move {
//!     QuotaScope::with(peer_meter, async move {
//!         let stacked = StackedMeteredProvider::from_current_scope(provider);
//!         stacked.chat_with_tools(...).await  // charges BOTH meters
//!     }).await
//! }).await
//! ```
//!
//! [`QuotaScope::current`] returns the innermost meter;
//! [`QuotaScope::collect_stack`] returns the full vec. Single-dimension
//! callers see a stack of length 1 and continue to work unchanged.
//!
//! ## Cross-spawn propagation
//!
//! `tokio::task_local!` is per-task, NOT inherited by `tokio::spawn`.
//! A scope opened at the run entrypoint does **not** automatically
//! cover spawned children — the compactor worker and the subagent
//! executor must each re-open `QuotaScope::with` inside their
//! spawned futures. (Confirmed by `scope_does_not_propagate_across_spawn`.)

use std::future::Future;
use std::sync::Arc;

use super::meter::QuotaMeter;

tokio::task_local! {
    /// Stack of quota meters active in this task tree. Each
    /// [`QuotaScope::with`] call appends one meter; the innermost
    /// (most recently pushed) meter is the most specific one for the
    /// current call site. Single-meter call sites see a stack of
    /// length 1; nested call sites (F20: principal + peer) see a
    /// stack of length ≥ 2.
    ///
    /// Read by:
    /// - [`MeteredProvider::from_current_scope`](crate::providers::MeteredProvider::from_current_scope)
    ///   — reads the innermost meter (single-dimension callers).
    /// - [`StackedMeteredProvider::from_current_scope`](crate::providers::StackedMeteredProvider::from_current_scope)
    ///   — reads the full stack via [`QuotaScope::collect_stack`].
    pub(crate) static QUOTA_METER_STACK: Vec<Arc<QuotaMeter>>;
}

/// Run a future with a quota meter bound as the current task-local.
///
/// Every [`MeteredProvider`](crate::providers::MeteredProvider) built
/// inside the await tree of `fut` will charge `meter` for its LLM
/// usage. No caller inside the scope has to remember to do anything
/// — that's the whole point.
///
/// # Nesting (F20)
///
/// `with` appends to the active meter stack rather than replacing it.
/// A call site that opens `QuotaScope::with(principal_meter, ...)`
/// followed by `QuotaScope::with(peer_meter, ...)` produces a stack
/// `[principal, peer]`. [`Self::current`] returns the innermost (peer),
/// and [`Self::collect_stack`] returns the full vec.
///
/// # Cross-spawn semantics
///
/// `tokio::task_local!` is per-task and does NOT propagate across
/// `tokio::spawn`. A scope opened at the run entrypoint does not
/// automatically cover spawned children — the compactor worker and
/// subagent executor must each re-open `QuotaScope::with` inside
/// their spawned futures.
pub struct QuotaScope;

impl QuotaScope {
    /// Run `fut` with `meter` pushed onto the current task-local
    /// meter stack.
    pub async fn with<F, T>(meter: Arc<QuotaMeter>, fut: F) -> T
    where
        F: Future<Output = T>,
    {
        let existing = QUOTA_METER_STACK.try_with(|s| s.clone()).unwrap_or_default();
        let mut next = existing;
        next.push(meter);
        QUOTA_METER_STACK.scope(next, fut).await
    }

    /// Read the innermost task-local meter, if any. Returns `None` if
    /// no [`QuotaScope::with`] is active in this task tree.
    ///
    /// Used by [`MeteredProvider::from_current_scope`](crate::providers::MeteredProvider::from_current_scope).
    /// For stacked callers (F20), use [`Self::collect_stack`] instead.
    #[must_use]
    pub fn current() -> Option<Arc<QuotaMeter>> {
        QUOTA_METER_STACK
            .try_with(|s| s.last().cloned())
            .ok()
            .flatten()
    }

    /// Read the full active meter stack (outermost first, innermost
    /// last). Returns an empty vec if no [`QuotaScope::with`] is
    /// active.
    ///
    /// Used by [`StackedMeteredProvider::from_current_scope`](crate::providers::StackedMeteredProvider::from_current_scope)
    /// to charge every meter in the stack on every LLM call.
    #[must_use]
    pub fn collect_stack() -> Vec<Arc<QuotaMeter>> {
        QUOTA_METER_STACK.try_with(|s| s.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::quota::QuotaConfig;

    #[tokio::test]
    async fn with_makes_meter_current_inside_scope() {
        let meter = Arc::new(QuotaMeter::load_or_init(
            QuotaConfig::default(),
            None,
            Utc::now(),
        ).await.unwrap());

        QuotaScope::with(meter.clone(), async {
            let current = QuotaScope::current();
            assert!(current.is_some(), "scope must be active");
            assert!(Arc::ptr_eq(&current.unwrap(), &meter));
        }).await;
    }

    #[tokio::test]
    async fn current_returns_none_outside_scope() {
        // No scope active in this task.
        assert!(QuotaScope::current().is_none());
    }

    #[tokio::test]
    async fn scope_does_not_leak_after_await() {
        let meter = Arc::new(QuotaMeter::load_or_init(
            QuotaConfig::default(),
            None,
            Utc::now(),
        ).await.unwrap());

        QuotaScope::with(meter, async {
            // After the inner scope returns, no scope is active.
        }).await;
        assert!(QuotaScope::current().is_none());
    }

    #[tokio::test]
    async fn nested_scope_uses_inner_meter() {
        let outer = Arc::new(QuotaMeter::load_or_init(
            QuotaConfig::default(),
            None,
            Utc::now(),
        ).await.unwrap());
        let inner = Arc::new(QuotaMeter::load_or_init(
            QuotaConfig::default(),
            None,
            Utc::now(),
        ).await.unwrap());

        QuotaScope::with(outer.clone(), async {
            assert!(Arc::ptr_eq(&QuotaScope::current().unwrap(), &outer));
            QuotaScope::with(inner.clone(), async {
                // Inner scope shadows outer — current() returns inner.
                assert!(Arc::ptr_eq(&QuotaScope::current().unwrap(), &inner));
            }).await;
            // Back to outer.
            assert!(Arc::ptr_eq(&QuotaScope::current().unwrap(), &outer));
        }).await;
    }

    #[tokio::test]
    async fn scope_propagates_through_await() {
        // The task-local survives `.await` points within the same task.
        let meter = Arc::new(QuotaMeter::load_or_init(
            QuotaConfig::default(),
            None,
            Utc::now(),
        ).await.unwrap());

        QuotaScope::with(meter.clone(), async {
            // Suspend and resume — scope must still be active.
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            let current = QuotaScope::current();
            assert!(current.is_some());
            assert!(Arc::ptr_eq(&current.unwrap(), &meter));
        }).await;
    }

    #[tokio::test]
    async fn scope_does_not_propagate_across_spawn() {
        // `tokio::task_local!` is per-task; `tokio::spawn` creates
        // a new task that does NOT inherit the parent's task-local.
        // The compactor worker and subagent executor must each
        // re-open `QuotaScope::with` inside their spawned futures.
        let meter = Arc::new(QuotaMeter::load_or_init(
            QuotaConfig::default(),
            None,
            Utc::now(),
        ).await.unwrap());

        let observed_in_spawn = Arc::new(std::sync::Mutex::new(true));

        QuotaScope::with(meter, async {
            // Spawn from inside the scope.
            let observed = Arc::clone(&observed_in_spawn);
            let _handle = tokio::spawn(async move {
                *observed.lock().unwrap() = QuotaScope::current().is_some();
            }).await.unwrap();
        }).await;

        // Inside the spawned task, no scope was active.
        assert!(
            !*observed_in_spawn.lock().unwrap(),
            "spawned task should not see scope"
        );
    }

    /// F20: `collect_stack` returns the empty vec when no scope is
    /// active.
    #[tokio::test]
    async fn collect_stack_is_empty_outside_scope() {
        assert!(QuotaScope::collect_stack().is_empty());
        assert!(QuotaScope::current().is_none());
    }

    /// F20: nested `with` calls produce a stack in the order they
    /// were opened. `current()` returns the innermost; `collect_stack()`
    /// returns the full vec with the outermost first.
    #[tokio::test]
    async fn collect_stack_walks_nested_scopes_in_order() {
        let outer = Arc::new(QuotaMeter::load_or_init(
            QuotaConfig::default(),
            None,
            Utc::now(),
        ).await.unwrap());
        let inner = Arc::new(QuotaMeter::load_or_init(
            QuotaConfig::default(),
            None,
            Utc::now(),
        ).await.unwrap());

        QuotaScope::with(outer.clone(), async {
            QuotaScope::with(inner.clone(), async {
                let stack = QuotaScope::collect_stack();
                assert_eq!(stack.len(), 2);
                assert!(Arc::ptr_eq(&stack[0], &outer));
                assert!(Arc::ptr_eq(&stack[1], &inner));
                // current() returns the innermost (most recent push).
                let cur = QuotaScope::current().unwrap();
                assert!(Arc::ptr_eq(&cur, &inner));
            }).await;

            // After the inner scope returns, only the outer remains.
            let stack = QuotaScope::collect_stack();
            assert_eq!(stack.len(), 1);
            assert!(Arc::ptr_eq(&stack[0], &outer));
        }).await;

        // After both scopes return, the stack is empty.
        assert!(QuotaScope::collect_stack().is_empty());
    }

    /// F20: deeply nested scopes (3+ levels) keep the full chain.
    /// Foreshadows future dimensions (org → tenant → principal → peer).
    #[tokio::test]
    async fn collect_stack_preserves_three_levels() {
        let a = Arc::new(QuotaMeter::load_or_init(QuotaConfig::default(), None, Utc::now()).await.unwrap());
        let b = Arc::new(QuotaMeter::load_or_init(QuotaConfig::default(), None, Utc::now()).await.unwrap());
        let c = Arc::new(QuotaMeter::load_or_init(QuotaConfig::default(), None, Utc::now()).await.unwrap());

        QuotaScope::with(a.clone(), async {
            QuotaScope::with(b.clone(), async {
                QuotaScope::with(c.clone(), async {
                    let stack = QuotaScope::collect_stack();
                    assert_eq!(stack.len(), 3);
                    assert!(Arc::ptr_eq(&stack[0], &a));
                    assert!(Arc::ptr_eq(&stack[1], &b));
                    assert!(Arc::ptr_eq(&stack[2], &c));
                }).await;
            }).await;
        }).await;
    }
}