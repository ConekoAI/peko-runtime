//! Parallel-execution gate for tool dispatch (F33, audit section 3 row 3).
//!
//! Mirrors codex's `codex-rs/core/src/tools/parallel.rs` shape — a single
//! `tokio::sync::RwLock<()>` per runtime, with parallelizable tools taking
//! the read-lock and non-parallelizable tools taking the write-lock.
//! Multiple parallelizable tools may dispatch concurrently; any
//! non-parallelizable tool serializes against every other running tool.
//!
//! ## Why a runtime-wide lock, not a per-tool-name map
//!
//! Codex's design is a single lock for the whole runtime, not a
//! `HashMap<String, RwLock<()>>` keyed by tool name. The reasoning:
//!
//! 1. The audit's "per-tool-name" framing is a slight misread of codex
//!    — codex uses one runtime-wide lock, not per-tool locks.
//! 2. A single lock is simpler and matches codex's semantics exactly.
//! 3. The "non-parallelizable tool blocks all others" behavior is what
//!    we want: `Write` racing with a concurrent `Bash` that reads the
//!    same file shouldn't happen.
//!
//! Per-path locking (e.g., "two Writes to different paths should run
//! concurrently, but Writes to the same path should serialize") is a
//! separate concern and would need a different mechanism (per-path
//! key set). Out of scope for F33 — the audit row is about parallel
//! races, not fine-grained resource locking.
//!
//! ## Pre-F33 problem
//!
//! Peko fans out every tool call in a single LLM response via
//! `try_join_all` (`engine/agentic_loop.rs:1740`). No coordination.
//! Two concurrent `Write` calls in the same batch clobber each
//! other; `Read + Write` to the same path can race; concurrent
//! `Bash` calls share cwd state. F33 adds the gate at the
//! `ToolExecutor::execute` chokepoint so built-ins and universal
//! tools share the serialization.

use std::sync::Arc;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Single runtime-wide gate. Cheap to clone via `Arc` so every tool
/// dispatch in the same agent shares the same gate instance.
#[derive(Clone, Debug)]
pub struct ParallelGate {
    inner: Arc<RwLock<()>>,
}

impl Default for ParallelGate {
    fn default() -> Self {
        Self::new()
    }
}

impl ParallelGate {
    /// Create a fresh gate. Call once per agent and hand clones to the
    /// `ToolExecutor`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(())),
        }
    }

    /// Acquire admission for a tool dispatch. `parallel = true` takes
    /// the read-lock (concurrent dispatches OK); `parallel = false`
    /// takes the write-lock (exclusive against every other running
    /// tool).
    ///
    /// The returned [`GateGuard`] must be held for the duration of the
    /// tool's actual work. Drop it when the tool dispatch returns.
    pub async fn admit(&self, parallel: bool) -> GateGuard<'_> {
        if parallel {
            GateGuard::Read(self.inner.read().await)
        } else {
            GateGuard::Write(self.inner.write().await)
        }
    }
}

/// RAII guard held for the duration of a tool dispatch. Drop = release
/// the gate slot.
///
/// Two-variant enum keeps the read/write guards behind a single Drop
/// boundary without taking on a `tokio_util::either::Either` dep just
/// for this.
pub enum GateGuard<'a> {
    /// Acquired by `Tool::parallelizable() == true`.
    Read(RwLockReadGuard<'a, ()>),
    /// Acquired by `Tool::parallelizable() == false`.
    Write(RwLockWriteGuard<'a, ()>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Three concurrent read-lock holders all run in parallel — peak
    /// in-flight count reaches 3. Proves the gate uses an RwLock and
    /// admits concurrent readers.
    #[tokio::test]
    async fn parallel_gate_read_locks_run_concurrently() {
        let gate = ParallelGate::new();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..3 {
            let gate = gate.clone();
            let in_flight = in_flight.clone();
            let peak = peak.clone();
            handles.push(tokio::spawn(async move {
                let _guard = gate.admit(true).await;
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let peak_observed = peak.load(Ordering::SeqCst);
        assert!(
            peak_observed >= 2,
            "expected concurrent read locks, peak in-flight was {peak_observed}"
        );
    }

    /// A held write lock blocks a second write attempt — proven by
    /// the second `admit(false)` timing out before the first is
    /// released.
    #[tokio::test]
    async fn parallel_gate_write_lock_serializes_writes() {
        let gate = ParallelGate::new();
        let write_held = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());

        let g1 = gate.clone();
        let wh = write_held.clone();
        let rf = release_first.clone();
        let first = tokio::spawn(async move {
            let _guard = g1.admit(false).await;
            wh.notify_one();
            rf.notified().await;
        });

        write_held.notified().await;

        // Second write attempt — must block because the first is held.
        let g2 = gate.clone();
        let blocked = tokio::time::timeout(Duration::from_millis(50), g2.admit(false)).await;
        assert!(
            blocked.is_err(),
            "second write should have timed out (write lock still held)"
        );

        release_first.notify_one();
        first.await.unwrap();

        // After release, a fresh write succeeds promptly.
        let g3 = gate.clone();
        let acquired = tokio::time::timeout(Duration::from_millis(50), g3.admit(false)).await;
        assert!(
            acquired.is_ok(),
            "third write should succeed after first releases"
        );
    }

    /// A held write lock blocks a read attempt. Proves the
    /// write-then-read ordering serializes correctly (this is what
    /// `Bash` + `Read` to the same file should look like post-F33).
    #[tokio::test]
    async fn parallel_gate_write_lock_blocks_reads() {
        let gate = ParallelGate::new();
        let write_held = Arc::new(tokio::sync::Notify::new());
        let release_write = Arc::new(tokio::sync::Notify::new());

        let g1 = gate.clone();
        let wh = write_held.clone();
        let rw = release_write.clone();
        let writer = tokio::spawn(async move {
            let _guard = g1.admit(false).await;
            wh.notify_one();
            rw.notified().await;
        });

        write_held.notified().await;

        // Reader attempt must block while the writer holds the gate.
        let g2 = gate.clone();
        let blocked = tokio::time::timeout(Duration::from_millis(50), g2.admit(true)).await;
        assert!(
            blocked.is_err(),
            "read should have timed out (write lock still held)"
        );

        release_write.notify_one();
        writer.await.unwrap();

        // After release, the reader proceeds.
        let g3 = gate.clone();
        let acquired = tokio::time::timeout(Duration::from_millis(50), g3.admit(true)).await;
        assert!(acquired.is_ok(), "read should succeed after write releases");
    }

    /// Default constructor yields a usable gate — guards are Send +
    /// 'static so the gate can move into spawned tasks.
    #[tokio::test]
    async fn parallel_gate_is_send_across_tasks() {
        let gate = ParallelGate::new();
        let g = gate.clone();
        let h = tokio::spawn(async move {
            let _guard = g.admit(true).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
        h.await.unwrap();
    }

    /// F33 wiring pin — a tool that doesn't override `parallelizable`
    /// admits with the read-lock (the default). Mirrors codex's
    /// `tool_supports_parallel` default.
    #[tokio::test]
    async fn parallel_gate_admits_default_parallelizable_tool_with_read_lock() {
        use crate::tools::Tool;
        struct DefaultTool;
        #[async_trait::async_trait]
        impl Tool for DefaultTool {
            fn name(&self) -> &str {
                "DefaultTool"
            }
            fn description(&self) -> String {
                String::new()
            }
            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                Ok(serde_json::json!({}))
            }
        }

        let gate = ParallelGate::new();
        let tool = DefaultTool;
        assert!(tool.parallelizable(), "default must be true");
        let _guard = gate.admit(tool.parallelizable()).await;
        // While this read-lock is held, another reader admits without
        // blocking — proves we got the read branch.
        let _guard2 = tokio::time::timeout(Duration::from_millis(50), gate.admit(true))
            .await
            .expect("second read-lock should admit immediately");
    }

    /// F33 wiring pin — a tool that overrides `parallelizable` to
    /// `false` admits with the write-lock. While the write-lock is
    /// held, no second admission succeeds within the test budget.
    #[tokio::test]
    async fn parallel_gate_admits_non_parallelizable_tool_with_write_lock() {
        use crate::tools::Tool;
        struct MutatingTool;
        #[async_trait::async_trait]
        impl Tool for MutatingTool {
            fn name(&self) -> &str {
                "MutatingTool"
            }
            fn description(&self) -> String {
                String::new()
            }
            fn parallelizable(&self) -> bool {
                false
            }
            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                Ok(serde_json::json!({}))
            }
        }

        let gate = ParallelGate::new();
        let tool = MutatingTool;
        assert!(!tool.parallelizable());
        let _guard = gate.admit(tool.parallelizable()).await;
        // Second admission of any kind must block — write-lock is
        // exclusive against both readers and writers.
        let blocked = tokio::time::timeout(Duration::from_millis(50), gate.admit(true)).await;
        assert!(
            blocked.is_err(),
            "read-lock attempt must block while write-lock is held"
        );
    }
}
