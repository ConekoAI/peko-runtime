//! Extension protocols - shared protocols for extension implementations
//!
//! This module contains protocol implementations used by extension adapters:
//! - `gateway`: Gateway IPC Protocol (stdio-line JSON for gateway extensions)
//! - `universal`: Universal Tool Protocol (JSON-RPC over stdio)
//! - `shared`: Common utilities used by multiple extension protocols (re-exported from framework)

pub mod gateway;

// `shared` module lives in the framework (src/extension/protocols/shared/)
// but is re-exported here for backward compatibility during Phase 1.
pub use crate::extension::protocols::shared as shared;

pub mod universal;

// Re-export shared utilities from the framework
pub use crate::extension::protocols::shared::{
    ContextResolver, ProcessConfig, ProcessTransport, ProcessTransportBuilder,
    filter_reserved_params, validate_no_reserved_params_leak, ValidationError,
    estimate_tool_duration, execute_with_context_handling, format_status,
};

// Re-export universal protocol types
pub use universal::{
    DescribeResult, ErrorObject, ExecuteParams, ExecuteResult, ExecutionContext, Manifest,
    ParamSource, ProtocolConfig, Request, ReservedParamsConfig, Response, ResponseResult,
    UniversalToolAdapter, UniversalToolBuilder, PROTOCOL_VERSION,
    load_and_register_tools, load_tools_from_directory,
    DiscoveredUniversalTool, ExtensionUniversalToolAdapter,
};
