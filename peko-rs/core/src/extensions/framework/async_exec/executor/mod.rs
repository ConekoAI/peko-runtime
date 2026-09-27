//! Async Tool Executor Framework
//!
//! Unified async tool execution with task lifecycle management,
//! result delivery, and file-based polling.
//!
//! This module consolidates the previously fragmented async tool
//! infrastructure (see Issue 006) into a single, tool-agnostic framework.
//!
//! Phase 8b: lifted from `src/extensions/framework/async_exec/executor/`
//! into `peko-extension-host`. Intra-crate paths use `crate::*`; the
//! previously-fractured `crate::extensions::framework::*` paths now
//! resolve through root re-export shims until Phase 16 deletes them.
//!
//! 2026-09-27 consolidation (ADR-063): the legacy delivery
//! stack is deleted — `queue.rs` (`AsyncResultQueueManager`), `delivery.rs`
//! (`QueueDelivery`/`ChannelDelivery`/`CallbackDelivery` + the formatter
//! registry), and `event_bus.rs` (`AsyncTaskEventBus`) had no live
//! consumers; completion delivery is exclusively the per-session inbox
//! push in `AsyncExecutor::execute_inner`, plus the idle-session wake
//! hook in [`wake`]. Task attribution (`AsyncToolConfig::principal_id`)
//! and a per-executor concurrency bound
//! ([`executor::DEFAULT_MAX_CONCURRENT_TASKS_PER_EXECUTOR`]) landed in the same pass.

pub mod async_runtime_impl;
pub mod completion_queue;
pub mod dispatch;
pub mod executor;
pub mod registry;
pub mod task_file;
pub mod types;
pub mod wake;

pub use async_runtime_impl::AsyncExecutorRuntime;
// Phase 8c.1.A: gated on `test-utils` feature so external root tests
// (src/tools/builtin/async_*.rs) can construct `TestAsyncRuntime` via
// the host's `test-utils` feature flag, not just host-internal tests.
#[cfg(any(test, feature = "test-utils"))]
pub use async_runtime_impl::{TestAsyncRuntime, TestTaskEntry};
pub use completion_queue::{
    CompletionEvent, InboxItem, SessionInbox, SharedSessionInbox, SteeringMessage,
};
pub use dispatch::ToolDispatchContext;
pub use executor::{
    default_inbox_factory, install_shared_inbox_registry, shared_inbox_registry,
    standalone_inbox_registry, AsyncExecutor, DEFAULT_MAX_CONCURRENT_TASKS_PER_EXECUTOR,
};
pub use registry::{
    cancel_task_across_all_registries, find_owning_registry_for_task,
    find_run_across_all_registries, find_task_across_all_registries,
    get_or_create_registry_for_agent, list_all_runs_across_all_registries,
    list_all_tasks_across_all_registries, AsyncTaskEntry, AsyncTaskRegistry, CancelResult,
    SharedAsyncTaskRegistry, SubagentMetadata, SubagentResult, TaskMetadata, TaskView,
};
pub use task_file::{TaskFileRecord, TaskFileWriter};
pub use types::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig, WaitResult,
};
pub use wake::{
    install_completion_wake_handler, uninstall_completion_wake_handler, CompletionWakeHandler,
    CompletionWakeNotice,
};
