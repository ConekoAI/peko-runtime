//! Extensions module — Extension Type Implementations
//!
//! This module contains **extension type implementations** (MCP, Gateway, Skill,
//! Builtin, General, Universal). The generic framework lives in `crate::extension`
//! (singular).
//!
//! During Phase 1 migration, this module re-exports all framework items from
//! `crate::extension` for backward compatibility. These re-exports will be
//! removed in Phase 4.

// ============================================================================
// Temporary backward-compatibility re-exports from framework (Phase 1)
// These will be removed in Phase 4.
// ============================================================================

// Re-export core types
pub use crate::extension::core::{
    common,
    binding::{HookBinding, HookBindingBuilder},
    config::{ExtensionConfig, ExtensionServices, TelemetryService},
    context::{HookContext, HookState},
    handler::{HookHandler, HookHandlerFactory},
    hook_points::{HookPoint, HookPointBuilder},
    registry::{global_core, init_global_core, ExtensionCore, RegisteredHook},
};

// Re-export types
pub use crate::extension::types::{
    AsyncReceipt, ExtensionId, ExtensionManifest, HookId, HookInput, HookOutput, HookPriority,
    HookResult, MessageEnvelope, PromptBuildState, SessionSnapshot, ToolMetadata,
    ToolRegistryAccess, ToolSource, DEFAULT_HOOK_PRIORITY, FALLBACK_HOOK_PRIORITY,
    SYSTEM_HOOK_PRIORITY, USER_HOOK_PRIORITY,
};

// Re-export services
pub use crate::extension::services::{
    ParamSource, ReservedParamsConfig, ReservedParamsService,
    Services as ExtensionServicesContainer, ToolExecutionConfig, ToolExecutionService,
};

// Re-export protocols
pub use crate::extension::protocols::shared::{
    ContextResolver, ProcessConfig, ProcessTransport, ProcessTransportBuilder,
    filter_reserved_params, validate_no_reserved_params_leak, ValidationError,
    estimate_tool_duration, execute_with_context_handling, format_status,
};

// Re-export framework submodules for backward compatibility
pub use crate::extension::async_exec;
pub use crate::extension::core;
pub use crate::extension::integration;
pub use crate::extension::manager;
pub use crate::extension::services;
pub use crate::extension::transport;
pub use crate::extension::types;

// ============================================================================
// Extension type implementations (staying in src/extensions/)
// ============================================================================

// Submodules for extension type implementations
pub mod adapters;
pub mod migration;
pub mod protocols;
pub mod runtime;

/// Extension type identifiers
pub mod extension_types {
    /// Skill extension type (SKILL.md)
    pub const SKILL: &str = "skill";

    /// MCP server extension type
    pub const MCP: &str = "mcp";

    /// Universal tool extension type
    pub const UNIVERSAL_TOOL: &str = "universal-tool";

    /// Gateway extension type
    pub const GATEWAY: &str = "gateway";

    /// Custom extension type prefix
    pub const CUSTOM_PREFIX: &str = "custom:";

    /// Check if a type is valid
    #[must_use]
    pub fn is_valid_type(ext_type: &str) -> bool {
        matches!(ext_type, SKILL | MCP | UNIVERSAL_TOOL | GATEWAY)
            || ext_type.starts_with(CUSTOM_PREFIX)
    }

    /// Get all standard extension types
    #[must_use]
    pub fn standard_types() -> Vec<&'static str> {
        vec![SKILL, MCP, UNIVERSAL_TOOL, GATEWAY]
    }
}

/// Prelude for convenient imports
pub mod prelude {
    pub use crate::extension::core::{
        common, ExtensionCore, HookContext, HookHandler, HookPoint, HookPointBuilder,
    };
    pub use crate::extension::types::{
        ExtensionId, ExtensionManifest, HookId, HookInput, HookOutput, HookResult,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_type_constants() {
        assert_eq!(extension_types::SKILL, "skill");
        assert_eq!(extension_types::MCP, "mcp");
        assert_eq!(extension_types::UNIVERSAL_TOOL, "universal-tool");
    }

    #[test]
    fn test_extension_type_validation() {
        assert!(extension_types::is_valid_type("skill"));
        assert!(extension_types::is_valid_type("mcp"));
        assert!(extension_types::is_valid_type("custom:internal"));
        assert!(!extension_types::is_valid_type("invalid"));
    }

    #[test]
    fn test_standard_types() {
        let types = extension_types::standard_types();
        assert!(types.contains(&"skill"));
        assert!(types.contains(&"mcp"));
        assert!(types.contains(&"gateway"));
    }
}
