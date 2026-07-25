//! Backwards-compat kitchen-sink for `protocols::shared::*` paths.
//!
//! Phase 8c.1 lifted all 4 child files into `peko_extension_host::protocols::shared`.
//! Phase 8c.2 deleted the root child files; this module collapses to the
//! re-exports callers historically used through the parent module path.
//!
//! New code should use `peko_extension_host::protocols::shared::X` directly.
//! This shim exists only for legacy `crate::extensions::framework::protocols
//! ::shared::X` callers; Phase 15 will delete it.

// Re-exports of the 3 lifted submodules' public surfaces (process_transport,
// proxy_utils, validation were the bits re-exported at the parent module
// pre-8c.1).
pub use peko_extension_host::protocols::shared::process_transport::{
    ProcessConfig, ProcessTransport, ProcessTransportBuilder,
};
pub use peko_extension_host::protocols::shared::proxy_utils::{
    estimate_tool_duration, execute_with_context_handling, format_status,
};
pub use peko_extension_host::protocols::shared::validation::{
    validate_no_reserved_params_leak, ValidationError,
};
// Reserved params re-exported from host's services module (Phase 7 lift).
pub use peko_extension_host::services::ParamSource as ReservedParamSource;
// Schema filter was lifted in Phase 8b.2; re-export its public surface here
// so the historical `crate::extensions::framework::protocols::shared::filter
// _reserved_params` path keeps resolving.
pub use peko_extension_host::protocols::shared::schema_filter::*;
