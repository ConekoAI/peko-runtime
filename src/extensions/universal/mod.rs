//! Universal Tool Extension Type Implementation
//!
//! This module contains all Universal Tool-specific code:
//! - `adapter`: ExtensionTypeAdapter for universal tools
//! - `protocol`: Universal Tool Protocol implementation

pub mod adapter;
pub mod protocol;

pub use crate::extensions::framework::services::ParamSource;
pub use crate::extensions::universal::protocol::manifest::ProtocolConfig;
pub use adapter::{load_tools_from_directory, DiscoveredUniversalTool, UniversalToolAdapter};
pub use protocol::{
    DescribeResult, ErrorObject, ExecuteParams, ExecuteResult, ExecutionContext, Manifest, Request,
    Response, ResponseResult, UniversalToolAdapter as ProtocolUniversalToolAdapter,
    UniversalToolBuilder, PROTOCOL_VERSION,
};
