//! Async execution infrastructure
//!
//! Background-task runtime used across the runtime: Bash background,
//! `AsyncSpawn`/`AsyncOutput`, cron firing, and messaging. Owns the
//! canonical `AsyncExecutor`, `CompletionQueue`, and the spawned-task
//! bookkeeping that engine flows events into, plus the cross-boundary
//! async-task inbox (`inbox`). Type-port helpers (`CompletionEvent`,
//! `SteeringMessage`) live in `peko_extension_api::completion_event`.

pub mod executor;
pub mod inbox;
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
