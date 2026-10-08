//! Tool installation, catalog lookup, execution, and built-in implementations.
//! Installation owns composition; ToolCatalog stores bindings and ToolDispatcher
//! validates and executes them with hooks and attributed audit events.

pub mod builtin;
pub mod catalog;
pub mod dispatcher;
pub mod installation;
pub mod prompt_sections;
pub mod runtime;
pub mod session_keys;

mod action_schema;
pub mod metadata;
