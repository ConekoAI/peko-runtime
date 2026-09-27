//! Async execution infrastructure for extensions
//!
//! This module provides async task execution capabilities used across
//! all extension types. Lifted from `src/extensions/framework/async_exec/`
//! in Phase 8b.

pub mod executor;
pub mod steer;

pub use executor::{
    cancel_task_across_all_registries, find_run_across_all_registries,
    find_task_across_all_registries, get_or_create_registry_for_agent,
    install_shared_inbox_registry, list_all_runs_across_all_registries,
    list_all_tasks_across_all_registries, shared_inbox_registry, standalone_inbox_registry,
    AsyncExecutor, AsyncTaskEntry, AsyncTaskId, AsyncTaskReceipt, AsyncTaskRegistry,
    AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig, SharedAsyncTaskRegistry, TaskFileRecord,
    TaskFileWriter, WaitResult,
};
pub use steer::format_cron_steer_message;
