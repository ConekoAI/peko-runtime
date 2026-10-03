//! Principal-scoped admission for live agent runs (ADR-067).
//!
//! Permits belong to execution futures, not registry status entries: a
//! cancelled run still occupies a slot until its future actually exits.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use peko_subject::PrincipalId;

use super::SpawnError;

pub const DEFAULT_MAX_RUNNING_AGENTS: NonZeroUsize = NonZeroUsize::new(20).unwrap();

/// One admission pool per principal in the explicitly shared runtime.
#[derive(Debug, Default)]
pub struct AgentRunLimits {
    principals: Mutex<HashMap<PrincipalId, Arc<AgentRunLimiter>>>,
}

impl AgentRunLimits {
    /// All entry paths and descendants of a principal receive the same pool.
    pub fn for_principal(&self, principal: &PrincipalId) -> Arc<AgentRunLimiter> {
        Arc::clone(
            self.principals
                .lock()
                .expect("agent run limits lock poisoned")
                .entry(principal.clone())
                .or_insert_with(|| Arc::new(AgentRunLimiter::default())),
        )
    }
}

#[derive(Debug)]
struct RunState {
    active: usize,
    max: NonZeroUsize,
}

/// Atomic, fail-fast admission. Waiting parents retain their permits.
#[derive(Debug)]
pub struct AgentRunLimiter {
    state: Mutex<RunState>,
}

impl Default for AgentRunLimiter {
    fn default() -> Self {
        Self {
            state: Mutex::new(RunState {
                active: 0,
                max: DEFAULT_MAX_RUNNING_AGENTS,
            }),
        }
    }
}

impl AgentRunLimiter {
    /// Update admission without interrupting existing runs. Lowering the
    /// limit below current usage refuses new runs until usage falls below it.
    pub fn set_limit(&self, max: NonZeroUsize) {
        self.state
            .lock()
            .expect("agent run limiter lock poisoned")
            .max = max;
    }

    pub fn active(&self) -> usize {
        self.state
            .lock()
            .expect("agent run limiter lock poisoned")
            .active
    }

    /// Reserve before session writes; transfer ownership to the run future.
    pub fn try_acquire(self: &Arc<Self>) -> Result<AgentRunPermit, SpawnError> {
        let mut state = self.state.lock().expect("agent run limiter lock poisoned");
        if state.active >= state.max.get() {
            return Err(SpawnError::ConcurrentLimitExceeded {
                current: state.active,
                max: state.max.get(),
            });
        }
        state.active += 1;
        Ok(AgentRunPermit(Arc::clone(self)))
    }
}

/// Releases admission on completion, failure, cancellation, timeout or unwind.
#[derive(Debug)]
#[must_use = "hold this permit until the agent execution future exits"]
pub struct AgentRunPermit(Arc<AgentRunLimiter>);

impl Drop for AgentRunPermit {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .expect("agent run limiter lock poisoned")
            .active -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_admission_never_exceeds_default_and_principals_are_isolated() {
        let limits = AgentRunLimits::default();
        let principal = PrincipalId::generate();
        let limiter = limits.for_principal(&principal);
        let start = Arc::new(std::sync::Barrier::new(81));
        let release = Arc::new(std::sync::Barrier::new(81));
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..80 {
                let pool = limits.for_principal(&principal);
                let start = start.clone();
                let release = release.clone();
                workers.push(scope.spawn(move || {
                    start.wait();
                    let permit = pool.try_acquire();
                    release.wait();
                    permit
                }));
            }
            start.wait();
            release.wait();
            let permits: Vec<_> = workers
                .into_iter()
                .filter_map(|w| w.join().unwrap().ok())
                .collect();
            assert_eq!(permits.len(), 20);
            assert_eq!(limiter.active(), 20);
            assert!(Arc::ptr_eq(&limiter, &limits.for_principal(&principal)));
            assert!(limits
                .for_principal(&PrincipalId::generate())
                .try_acquire()
                .is_ok());
            drop(permits);
        });
        assert_eq!(limiter.active(), 0);
    }

    #[tokio::test]
    async fn detached_cancelled_task_keeps_slot_until_its_future_exits() {
        use crate::async_exec::executor::{AsyncExecutor, AsyncToolConfig};
        let pool = Arc::new(AgentRunLimiter::default());
        pool.set_limit(NonZeroUsize::new(1).unwrap());
        let exec = AsyncExecutor::new(crate::async_exec::executor::standalone_inbox_registry());
        let permit = pool.try_acquire().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        // The receipt returns while execution is still in the background.
        exec.execute(
            "live-agent".into(),
            "Agent",
            serde_json::json!({}),
            "parent",
            AsyncToolConfig {
                timeout_secs: None,
                ..Default::default()
            },
            move || async move {
                let _run_permit = permit;
                started_tx.send(()).unwrap();
                finish_rx.await.unwrap();
                Ok(serde_json::json!("done"))
            },
        )
        .await
        .unwrap();
        started_rx.await.unwrap();
        exec.cancel(&"live-agent".into()).await.unwrap();
        assert_eq!(pool.active(), 1);
        assert!(pool.try_acquire().is_err());
        finish_tx.send(()).unwrap();
        wait_until_released(&pool).await;
        assert!(pool.try_acquire().is_ok());
    }

    #[tokio::test]
    async fn timeout_and_aborted_execution_release_capacity() {
        use crate::async_exec::executor::{AsyncExecutor, AsyncToolConfig};
        let pool = Arc::new(AgentRunLimiter::default());
        let exec = AsyncExecutor::new(crate::async_exec::executor::standalone_inbox_registry());
        let permit = pool.try_acquire().unwrap();
        exec.execute(
            "timed-agent".into(),
            "Agent",
            serde_json::json!({}),
            "parent",
            AsyncToolConfig {
                timeout_millis: Some(10),
                ..Default::default()
            },
            move || async move {
                let _run_permit = permit;
                std::future::pending::<anyhow::Result<serde_json::Value>>().await
            },
        )
        .await
        .unwrap();
        wait_until_released(&pool).await;
        let permit = pool.try_acquire().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _run_permit = permit;
            started_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(pool.active(), 0);
    }

    #[test]
    fn lowering_limit_preserves_runs_and_prevents_new_admission() {
        let pool = Arc::new(AgentRunLimiter::default());
        let first = pool.try_acquire().unwrap();
        let second = pool.try_acquire().unwrap();
        pool.set_limit(NonZeroUsize::new(1).unwrap());
        assert!(matches!(
            pool.try_acquire(),
            Err(SpawnError::ConcurrentLimitExceeded { current: 2, max: 1 })
        ));
        drop(second);
        assert!(pool.try_acquire().is_err());
        drop(first);
        assert!(pool.try_acquire().is_ok());
    }

    async fn wait_until_released(pool: &AgentRunLimiter) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while pool.active() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
