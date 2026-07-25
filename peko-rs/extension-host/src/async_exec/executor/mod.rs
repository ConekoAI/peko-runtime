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

pub mod async_runtime_impl;
pub mod completion_queue;
pub mod delivery;
pub mod dispatch;
pub mod event_bus;
pub mod executor;
pub mod queue;
pub mod registry;
pub mod task_file;
pub mod types;

pub use async_runtime_impl::AsyncExecutorRuntime;
// Phase 8c.1.A: gated on `test-utils` feature so external root tests
// (src/tools/builtin/async_*.rs) can construct `TestAsyncRuntime` via
// the host's `test-utils` feature flag, not just host-internal tests.
#[cfg(any(test, feature = "test-utils"))]
pub use async_runtime_impl::{TestAsyncRuntime, TestTaskEntry};
pub use completion_queue::{
    CompletionEvent, InboxItem, SessionInbox, SharedSessionInbox, SteeringMessage,
};
pub use delivery::{
    build_completion_event, CallbackDelivery, ChannelDelivery, DefaultResultFormatter,
    FormatterRegistry, QueueDelivery, ResultDelivery, ResultFormatter,
};
pub use dispatch::ToolDispatchContext;
pub use event_bus::{AsyncTaskCompletionEvent, AsyncTaskEventBus};
pub use executor::AsyncExecutor;
pub use queue::{AsyncResultQueue, AsyncResultQueueManager, SharedAsyncResultQueueManager};
pub use registry::{
    cancel_task_across_all_registries, find_run_across_all_registries,
    find_task_across_all_registries, get_or_create_registry_for_agent,
    list_all_runs_across_all_registries, list_all_tasks_across_all_registries, AsyncTaskEntry,
    AsyncTaskRegistry, CancelResult, SharedAsyncTaskRegistry, SubagentMetadata, SubagentResult,
    TaskMetadata, TaskView,
};
pub use task_file::{TaskFileRecord, TaskFileWriter};
pub use types::{
    AsyncResultDeliveryMode, AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus,
    AsyncToolConfig, DeliveryTarget, SessionMessageType, WaitResult,
};
