//! General Extension Type Implementation
//!
//! This module contains the General Extension adapter for unconstrained hook-based extensions.

pub mod adapter;

pub use adapter::{
    discover_general_extensions, load_and_register_general_extensions,
    register_general_extensions_with_core, DiscoveredGeneralExtension, GeneralExtensionAdapter,
    GeneralExtensionConfig, HookDeclaration,
};
