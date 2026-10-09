//! Async Tool Executor Framework
//!
//! Unified async tool execution with task lifecycle management,
//! result delivery, and file-based polling.
//!
//! This module consolidates the previously fragmented async tool
//! infrastructure (see Issue 006) into a single, tool-agnostic framework.
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
pub use completion_queue::{
    CompletionEvent, InboxItem, SessionInbox, SharedSessionInbox, SteeringMessage,
};
pub use dispatch::ToolDispatchContext;
pub use executor::{
    default_inbox_factory, install_shared_inbox_registry, shared_inbox_registry,
    standalone_inbox_registry, AsyncExecutor, DEFAULT_MAX_CONCURRENT_TASKS_PER_EXECUTOR,
};
pub use registry::{
    AsyncTaskEntry, AsyncTaskRegistry, CancelResult, SharedAsyncTaskRegistry, SubagentMetadata,
    SubagentResult, TaskMetadata,
};
pub use task_file::{TaskFileRecord, TaskFileWriter};
pub use types::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig, WaitResult,
};
pub use wake::{
    install_completion_wake_handler, uninstall_completion_wake_handler, CompletionWakeHandler,
    CompletionWakeNotice,
};
