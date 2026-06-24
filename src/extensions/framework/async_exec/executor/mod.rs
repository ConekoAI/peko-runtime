//! Async Tool Executor Framework
//!
//! Unified async tool execution with task lifecycle management,
//! result delivery, and file-based polling.
//!
//! This module consolidates the previously fragmented async tool
//! infrastructure (see Issue 006) into a single, tool-agnostic framework.

pub mod completion_queue;
pub mod delivery;
pub mod event_bus;
pub mod executor;
pub mod queue;
pub mod registry;
pub mod task_file;
pub mod types;

pub use completion_queue::{
    AsyncTaskCompletionQueue, CompletionEvent, SharedAsyncTaskCompletionQueue,
};
pub use delivery::{
    build_completion_event, CallbackDelivery, ChannelDelivery, DefaultResultFormatter,
    FormatterRegistry, QueueDelivery, ResultDelivery, ResultFormatter,
};
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
