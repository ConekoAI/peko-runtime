//! Extension protocols - shared protocols for extension implementations
//!
//! This module contains protocol implementations used by extension adapters:
//! - `gateway`: Gateway IPC Protocol (stdio-line JSON for gateway extensions)
//! - `universal`: Universal Tool Protocol (JSON-RPC over stdio)
//! - `shared`: Common utilities used by multiple extension protocols

pub mod gateway;
pub mod shared;
pub mod universal;

pub use shared::{
    ContextResolver, ProcessConfig, ProcessTransport, ProcessTransportBuilder,
    filter_reserved_params, validate_no_reserved_params_leak, ValidationError,
    estimate_tool_duration, execute_with_context_handling, format_status,
};
pub use universal::{
    DescribeResult, ErrorObject, ExecuteParams, ExecuteResult, ExecutionContext, Manifest,
    ParamSource, ProtocolConfig, Request, ReservedParamsConfig, Response, ResponseResult,
    UniversalToolAdapter, UniversalToolBuilder, PROTOCOL_VERSION,
    load_and_register_tools, load_tools_from_directory,
    DiscoveredUniversalTool, ExtensionUniversalToolAdapter,
};
