//! Async execution infrastructure for extensions
//!
//! This module provides async task execution capabilities used across
//! all extension types.

pub mod executor;

pub use executor::{
    AsyncExecutor, AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent,
    AsyncTaskEntry, AsyncTaskEventBus, AsyncTaskId, AsyncTaskReceipt, AsyncTaskRegistry,
    AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig, CallbackDelivery, ChannelDelivery,
    DefaultResultFormatter, DeliveryTarget, FormatterRegistry, QueueDelivery, ResultDelivery,
    ResultFormatter, SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry,
    TaskFileRecord, TaskFileWriter, WaitResult,
    build_completion_event, cancel_task_across_all_registries, find_run_across_all_registries,
    find_task_across_all_registries, get_or_create_registry_for_agent,
    list_all_runs_across_all_registries, list_all_tasks_across_all_registries,
};
