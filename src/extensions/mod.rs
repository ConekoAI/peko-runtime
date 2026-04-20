//! Extensions module - Unified Extension Architecture
//!
//! This module implements the Unified Extension Architecture (ADR-017) which provides
//! a single, consistent way to extend Pekobot's capabilities.
//!
//! # Overview
//!
//! The architecture consists of three layers:
//!
//! 1. **Extension Core** (`core/`): Defines all hook points in the agentic loop
//!    and manages registration/invocation of handlers.
//!
//! 2. **Extension Type Adapters** (`adapters/`): Map specific extension formats
//!    (SKILL.md, MCP config, etc.) to hook points.
//!
//! 3. **Extension Manager** (`manager/`): Provides unified lifecycle management
//!    (install/enable/disable/uninstall/bundle) and CLI commands.
//!
//! # Extension Types
//!
//! | Type | Description | Hook Points |
//! |------|-------------|-------------|
//! | `skill` | Documentation-based skills | `PromptSystemSection(skills)` |
//! | `mcp` | MCP servers | `ToolRegister`, `PromptSystemSection(tools)`, `ToolExecute` |
//! | `universal-tool` | Universal tool protocol | `ToolRegister`, `PromptSystemSection(tools)`, `ToolExecute` |
//! | `gateway` | Messaging gateways | `ChannelInput`, `ChannelOutput`, `ToolRegister`, `EventEmit` |
//!
//! # Quick Start
//!
//! ## Using the Extension Core directly
//!
//! ```rust,ignore
//! use pekobot::extensions::{
//!     ExtensionCore,
//!     HookPoint,
//!     HookHandler,
//!     HookContext,
//!     HookResult,
//! };
//!
//! // Create and register a handler
//! let core = ExtensionCore::new();
//! let handler = Arc::new(MyHandler);
//!
//! core.register_hook(
//!     HookPoint::PromptSystemSection { section: "custom".to_string(), priority: 100 },
//!     handler,
//!     &ExtensionId::new("my-extension"),
//! ).await?;
//!
//! // Invoke hooks
//! let result = core.invoke_hook(HookPoint::ToolRegister, HookInput::Unit).await;
//! ```
//!
//! ## Using the Extension Manager (future)
//!
//! ```bash
//! # Install an extension
//! pekobot ext install ./my-skill
//!
//! # List extensions
//! pekobot ext list
//!
//! # Enable/disable
//! pekobot ext enable my-skill
//! pekobot ext disable my-skill
//! ```

// Re-export core types
pub use core::{
    common,
    context::{
        ExtensionConfig, ExtensionServices, HookBinding, HookBindingBuilder, HookContext,
        HookHandler, HookHandlerFactory, HookState, TelemetryService,
    },
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

// Submodules
pub mod adapters;
pub mod async_integration;
pub mod core;
pub mod manager;
pub mod migration;
pub mod services;
pub mod types;

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
    pub use super::core::{
        common, ExtensionCore, HookContext, HookHandler, HookPoint, HookPointBuilder,
    };
    pub use super::types::{
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
