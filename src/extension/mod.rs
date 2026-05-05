//! Extension Framework — Generic Extension Core (ADR-017)
//!
//! This module contains the **generic extension framework** — hook points,
//! registries, types, managers, and shared services. It has **zero dependencies**
//! on extension type implementations.
//!
//! Extension type implementations (MCP, Gateway, Skill, etc.) live in
//! `crate::extensions` (plural), not here.

// Re-export core types
pub use core::{
    common,
    binding::{HookBinding, HookBindingBuilder},
    config::{ExtensionConfig, ExtensionServices, TelemetryService},
    context::{HookContext, HookState},
    handler::{HookHandler, HookHandlerFactory},
    hook_points::{HookPoint, HookPointBuilder},
    registry::{global_core, init_global_core, ExtensionCore, RegisteredHook},
};

// Re-export types
pub use types::{
    AsyncReceipt, ExtensionId, ExtensionManifest, HookId, HookInput, HookOutput, HookPriority,
    HookResult, MessageEnvelope, PromptBuildState, SessionSnapshot, ToolMetadata,
    ToolRegistryAccess, ToolSource, DEFAULT_HOOK_PRIORITY, FALLBACK_HOOK_PRIORITY,
    SYSTEM_HOOK_PRIORITY, USER_HOOK_PRIORITY,
};

// Re-export services
pub use services::{
    ParamSource, ReservedParamsConfig, ReservedParamsService,
    Services as ExtensionServicesContainer, ToolExecutionConfig, ToolExecutionService,
};

// Re-export protocols shared utilities
pub use protocols::shared::{
    ContextResolver, ProcessConfig, ProcessTransport, ProcessTransportBuilder,
    filter_reserved_params, validate_no_reserved_params_leak, ValidationError,
    estimate_tool_duration, execute_with_context_handling, format_status,
};

// Submodules
pub mod async_exec;
pub mod core;
pub mod integration;
pub mod manager;
pub mod protocols;
pub mod services;
pub mod transport;
pub mod types;

/// Prelude for convenient imports
pub mod prelude {
    pub use super::core::{
        common, ExtensionCore, HookContext, HookHandler, HookPoint, HookPointBuilder,
    };
    pub use super::types::{
        ExtensionId, ExtensionManifest, HookId, HookInput, HookOutput, HookResult,
    };
}
