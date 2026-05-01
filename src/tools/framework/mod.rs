//! Tool execution frameworks and protocols
//!
//! This module contains reusable frameworks for tool execution:
//! - `async_executor`: Async task lifecycle management
//! - `universal`: Universal Tool Protocol (JSON-RPC over stdio)
//! - `shared`: Common utilities used by both Universal Tools and MCP tools

pub mod async_executor;
pub mod shared;
pub mod universal;

pub use async_executor::{
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

pub use shared::{
    ContextResolver, ProcessConfig, ProcessTransport, ProcessTransportBuilder,
    ReservedParamSource, filter_reserved_params, validate_no_reserved_params_leak,
    ValidationError, estimate_tool_duration, execute_with_context_handling, format_status,
};

pub use universal::{
    DescribeResult, ErrorObject, ExecuteParams, ExecuteResult, ExecutionContext, Manifest,
    ParamSource, ProtocolConfig, Request, ReservedParamsConfig, Response, ResponseResult,
    UniversalToolAdapter, UniversalToolBuilder, PROTOCOL_VERSION,
    load_and_register_tools, load_tools_from_directory,
    DiscoveredUniversalTool, ExtensionUniversalToolAdapter,
};
