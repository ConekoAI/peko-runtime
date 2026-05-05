//! Universal Tool Protocol
//!
//! JSON-RPC over stdio protocol for universal tools.

pub mod adapter;
pub mod manifest;
pub mod protocol;
pub mod transport;

#[cfg(test)]
mod tests;

pub use adapter::{
    UniversalToolAdapter, UniversalToolBuilder,
};
pub use manifest::Manifest;
pub use protocol::{
    DescribeResult, ErrorObject, ExecuteParams, ExecuteResult, ExecutionContext,
    Request, Response, ResponseResult, PROTOCOL_VERSION,
};
pub use crate::extension::services::ParamSource;
pub use crate::extensions::universal::protocol::manifest::ProtocolConfig;
pub use transport::Transport as UniversalToolTransport;
