//! Universal Tool runtime (ADR-047 §2.1, §2.4).
//!
//! Phase 2 PR 3 deletes the framework-coupled
//! `ExtensionTypeAdapter` impl. What remains:
//! - `protocol`: the universal tool JSON-RPC-over-stdio protocol
//!   (`Manifest`, `Request`, `Response`, `Transport`, `UniversalToolAdapter`
//!   which implements `peko_tools_core::Tool`).
//! - `workspace`: workspace scanner that walks
//!   `<workspace>/tools/<id>/manifest.yaml` and registers each
//!   tool via [`crate::extensions::builtin::BuiltinToolAdapter::register_tool`]
//!   — the canonical path; no framework hook layer.
//!
//! Removed: `adapter` (581 lines), the four auto-generated framework
//! hooks per tool, and `load_tools_from_directory` /
//! `load_and_register_tools` framework plumbing. Workspace-resident
//! universal tools are now a `BuiltinToolAdapter` registration away.

pub mod protocol;
pub mod workspace;

pub use crate::extensions::universal::protocol::manifest::ProtocolConfig;
pub use protocol::{
    DescribeResult, ErrorObject, ExecuteParams, ExecuteResult, ExecutionContext, Manifest, Request,
    Response, ResponseResult, UniversalToolAdapter, UniversalToolBuilder, PROTOCOL_VERSION,
};
pub use workspace::{
    discover_workspace_universal_tools, load_workspace_universal_tools,
};
