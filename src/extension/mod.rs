//! Extension Framework — Generic Extension Core (ADR-017)
//!
//! This module contains the **generic extension framework** — hook points,
//! registries, types, managers, and shared services. It has **zero dependencies**
//! on extension type implementations.
//!
//! Extension type implementations (MCP, Gateway, Skill, etc.) live in
//! `crate::extensions` (plural), not here.
//!
//! # Module Boundaries
//!
//! This module (`src/extension/`) must NOT import from:
//! - `crate::extensions` (extension type implementations)
//! - `crate::mcp` (absorbed into `crate::extensions::mcp`)
//! - `crate::daemon` (daemon-specific code)
//! - `crate::tools` (tool implementations)
//!
//! Dependency direction: `extension::core` → `extension::types` → `extension::manager|services|protocols|async_exec|transport`

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

// ============================================================================
// Framework Submodules
// ============================================================================

/// Extension type adapter trait, manifest formats, and built-in adapter provider.
pub mod adapters;

/// Async task execution framework.
pub mod async_exec;

/// Hook points, registry, handler traits — the core of the extension system.
pub mod core;

/// Extension integration layer (tool bridge).
pub mod integration;

/// Extension lifecycle management (install, enable, disable, discover, bundle).
pub mod manager;

/// Shared protocol utilities (process transport, validation, schema filter).
pub mod protocols;

/// Param injection, tool execution, validation.
pub mod services;

/// Async task transport layer.
pub mod transport;

/// Extension type definitions (ExtensionManifest, HookResult, etc.).
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
