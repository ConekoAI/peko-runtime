//! Async execution infrastructure for extensions
//!
//! This module provides async task execution capabilities used across
//! all extension types. Phase 8b.2 deleted the `steer` root shim and now
//! re-exports `format_cron_steer_message` directly from `peko_extension_host`.

pub mod executor;
pub mod steer {
    //! Re-export of host-side steer helpers (Phase 8b.2).
    pub use peko_extension_host::async_exec::steer::format_cron_steer_message;
}

pub use executor::{
    build_completion_event, cancel_task_across_all_registries, find_run_across_all_registries,
    find_task_across_all_registries, get_or_create_registry_for_agent,
    list_all_runs_across_all_registries, list_all_tasks_across_all_registries, AsyncExecutor,
    AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent, AsyncTaskEntry,
    AsyncTaskEventBus, AsyncTaskId, AsyncTaskReceipt, AsyncTaskRegistry, AsyncTaskResult,
    AsyncTaskStatus, AsyncToolConfig, CallbackDelivery, ChannelDelivery, DefaultResultFormatter,
    DeliveryTarget, FormatterRegistry, QueueDelivery, ResultDelivery, ResultFormatter,
    SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry, TaskFileRecord,
    TaskFileWriter, WaitResult,
};
pub use peko_extension_host::async_exec::steer::format_cron_steer_message;
