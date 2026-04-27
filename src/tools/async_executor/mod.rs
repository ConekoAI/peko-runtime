//! Async Tool Executor Framework
//!
//! Unified async tool execution with task lifecycle management,
//! result delivery, and file-based polling.
//!
//! This module consolidates the previously fragmented async tool
//! infrastructure (see Issue 006) into a single, tool-agnostic framework.

pub mod delivery;
pub mod event_bus;
pub mod executor;
pub mod queue;
pub mod registry;
pub mod task_file;
pub mod types;

pub use delivery::{
    build_completion_event, CallbackDelivery, ChannelDelivery, DefaultResultFormatter,
    FormatterRegistry, QueueDelivery, ResultDelivery, ResultFormatter,
};
pub use event_bus::{AsyncTaskCompletionEvent, AsyncTaskEventBus};
pub use executor::AsyncExecutor;
pub use queue::{AsyncResultQueue, AsyncResultQueueManager, SharedAsyncResultQueueManager};
pub use registry::{AsyncTaskEntry, AsyncTaskRegistry, SharedAsyncTaskRegistry};
pub use task_file::{TaskFileRecord, TaskFileWriter};
pub use types::{
    AsyncResultDeliveryMode, AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus,
    AsyncToolConfig, DeliveryTarget, SessionMessageType, WaitResult,
};
