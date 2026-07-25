//! General Extension Type Implementation
//!
//! This module contains the General Extension adapter for unconstrained hook-based extensions.
//! Hook declarations may optionally declare a `command` that is executed when the hook fires;
//! the command's stdout is parsed and injected as hook output (e.g. `session.start` bootstrap context).

pub mod adapter;
pub mod command_handler;

pub use adapter::{
    discover_general_extensions, load_and_register_general_extensions,
    register_general_extensions_with_core, DiscoveredGeneralExtension, GeneralExtensionAdapter,
    GeneralExtensionConfig, HookDeclaration,
};
pub use command_handler::{
    CommandHookConfig, CommandHookHandler, CommandOutputFormat, DEFAULT_COMMAND_TIMEOUT_SECS,
    MAX_COMMAND_OUTPUT_BYTES,
};
