//! Async execution infrastructure
//!
//! Background-task runtime used across the runtime: Bash background,
//! `Async action spawn`/`Async action output`, cron firing, and messaging. Owns the
//! canonical `AsyncExecutor`, `CompletionQueue`, and the spawned-task
//! bookkeeping that engine flows events into, plus the cross-boundary
//! async-task inbox (`inbox`). Type-port helpers (`CompletionEvent`,
//! `SteeringMessage`) live in `peko_session::completion_event`.

pub mod executor;
pub mod inbox;
pub mod steer;

pub use executor::{
    install_shared_inbox_registry, shared_inbox_registry, standalone_inbox_registry, AsyncExecutor,
    AsyncTaskEntry, AsyncTaskId, AsyncTaskReceipt, AsyncTaskRegistry, AsyncTaskResult,
    AsyncTaskStatus, AsyncToolConfig, SharedAsyncTaskRegistry, TaskFileRecord, TaskFileWriter,
    WaitResult,
};
pub use steer::format_cron_steer_message;
