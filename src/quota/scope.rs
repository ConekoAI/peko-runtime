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
    /// The current principal's quota meter. Set by [`QuotaScope::with`]
    /// and read by [`MeteredProvider`](crate::providers::MeteredProvider)
    /// at construction time.
    pub(crate) static QUOTA_METER: Arc<QuotaMeter>;
}

/// Run a future with a quota meter bound as the current task-local.
///
/// Every [`MeteredProvider`](crate::providers::MeteredProvider) built
/// inside the await tree of `fut` will charge `meter` for its LLM
/// usage. No caller inside the scope has to remember to do anything
/// — that's the whole point.
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
    /// Run `fut` with `meter` bound as the current task-local meter.
    pub async fn with<F, T>(meter: Arc<QuotaMeter>, fut: F) -> T
    where
        F: Future<Output = T>,
    {
        QUOTA_METER.scope(meter, fut).await
    }

    /// Read the current task-local meter, if any. Returns `None` if
    /// no [`QuotaScope::with`] is active in this task tree.
    ///
    /// Used by [`MeteredProvider::from_current_scope`](crate::providers::MeteredProvider::from_current_scope).
    #[must_use]
    pub fn current() -> Option<Arc<QuotaMeter>> {
        QUOTA_METER.try_with(|m| m.clone()).ok()
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
}