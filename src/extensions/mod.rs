//! Extensions module — Extension Framework + Type Implementations
//!
//! Contains both the **generic extension framework** (under `framework/`)
//! and the **extension type implementations** (MCP, Gateway, Skill, Builtin,
//! General, Universal). The framework is generic and dependency-free; type
//! implementations sit beside it and depend on the framework.
//!
//! # Module Boundaries
//!
//! Each extension type lives in its own directory with its adapter, runtime,
//! and protocol code. Cross-extension dependencies should go through the
//! framework (`crate::extensions::framework`), not directly between extension types.
//!
//! Extension types must NOT be added to this module's submodules without
//! also providing an `ExtensionTypeAdapter` implementation.
//!
//! # Directory Layout
//!
//! ```text
//! src/extensions/
//! ├── framework/   # Generic framework: core, adapters, manager, types, transport, services, protocols, scaffold, async_exec
//! ├── builtin/     # Built-in tool adapter
//! ├── gateway/     # Gateway adapter, protocol, runtime
//! ├── general/     # General extension adapter
//! ├── mcp/         # MCP adapter, protocol, runtime
//! ├── skill/       # Skill adapter
//! └── universal/   # Universal tool adapter and protocol
//! ```

// ============================================================================
// Framework
// ============================================================================

/// Generic extension framework (core, adapters, manager, types, transport,
/// services, protocols, scaffold, async_exec). Zero dependencies on
/// extension type implementations. Extension type adapters depend on this;
/// this module must not depend on its sibling extension type submodules.
pub mod framework;

// ============================================================================
// Extension Type Submodules
// ============================================================================

/// Built-in tool adapter — registers native Tool trait implementations with ExtensionCore.
pub mod builtin;

/// Gateway extension — platform gateway adapters (HTTP, WebSocket, pub/sub).
pub mod gateway;

/// General extension adapter — unconstrained access to all 22 hook points.
pub mod general;

/// MCP extension — Model Context Protocol server integration.
pub mod mcp;

/// Skill extension adapter — SKILL.md-based capabilities with YAML frontmatter.
pub mod skill;

/// Agent extension adapter — AGENT.md-based prompt extensions with YAML frontmatter.
pub mod agent;

/// Slash command extension adapter — COMMAND.md-based user-invoked commands with YAML frontmatter.
pub mod slash;

/// Universal tool extension — external executable tools with manifest.yaml.
pub mod universal;

/// Manifest validation service — walks an extension directory, detects its
/// type (Tier 1 ecosystem standard or Tier 2 unified manifest), and runs
/// optional semantic checks (ADR-036). Lives here next to the extension
/// types it inspects rather than in the framework, so the framework can
/// stay free of `crate::extensions::*` dependencies.
pub mod validation;

// ============================================================================
// Utilities
// ============================================================================

// ============================================================================
// Built-in Adapter Provider
// ============================================================================

use std::sync::Arc;

/// Built-in adapter provider
///
/// Constructs all built-in extension type adapters. Lives in `src/extensions/`
/// (plural) because it depends on all extension type implementations.
pub struct BuiltInAdapters;

impl BuiltInAdapters {
    pub fn new() -> Self {
        Self
    }

    pub fn adapters(
        &self,
    ) -> Vec<Box<dyn crate::extensions::framework::adapters::ExtensionTypeAdapter>> {
        vec![
            Box::new(skill::adapter::SkillAdapter::new()),
            Box::new(agent::adapter::AgentAdapter::new()),
            Box::new(slash::adapter::SlashAdapter::new()),
            Box::new(universal::adapter::UniversalToolAdapter::new()),
            Box::new(mcp::adapter::McpAdapter::with_default_manager()),
            Box::new(gateway::adapter::GatewayAdapter::new(Arc::new(
                crate::extensions::framework::core::ExtensionCore::new(),
            ))),
            Box::new(general::adapter::GeneralExtensionAdapter::new()),
        ]
    }
}

impl Default for BuiltInAdapters {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension type identifiers and validation.
pub mod extension_types {
    /// Skill extension type (SKILL.md)
    pub const SKILL: &str = "skill";

    /// Agent extension type (AGENT.md)
    pub const AGENT: &str = "agent";

    /// MCP server extension type
    pub const MCP: &str = "mcp";

    /// Slash command extension type
    pub const SLASH: &str = "slash";

    /// Universal tool extension type
    pub const UNIVERSAL_TOOL: &str = "universal-tool";

    /// Gateway extension type
    pub const GATEWAY: &str = "gateway";

    /// Custom extension type prefix
    pub const CUSTOM_PREFIX: &str = "custom:";

    /// Check if a type is valid
    #[must_use]
    pub fn is_valid_type(ext_type: &str) -> bool {
        matches!(
            ext_type,
            SKILL | AGENT | SLASH | MCP | UNIVERSAL_TOOL | GATEWAY
        ) || ext_type.starts_with(CUSTOM_PREFIX)
    }

    /// Get all standard extension types
    #[must_use]
    pub fn standard_types() -> Vec<&'static str> {
        vec![SKILL, AGENT, SLASH, MCP, UNIVERSAL_TOOL, GATEWAY]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_type_constants() {
        assert_eq!(extension_types::SKILL, "skill");
        assert_eq!(extension_types::AGENT, "agent");
        assert_eq!(extension_types::SLASH, "slash");
        assert_eq!(extension_types::MCP, "mcp");
        assert_eq!(extension_types::UNIVERSAL_TOOL, "universal-tool");
    }

    #[test]
    fn test_extension_type_validation() {
        assert!(extension_types::is_valid_type("skill"));
        assert!(extension_types::is_valid_type("agent"));
        assert!(extension_types::is_valid_type("slash"));
        assert!(extension_types::is_valid_type("mcp"));
        assert!(extension_types::is_valid_type("custom:internal"));
        assert!(!extension_types::is_valid_type("invalid"));
    }

    #[test]
    fn test_standard_types() {
        let types = extension_types::standard_types();
        assert!(types.contains(&"skill"));
        assert!(types.contains(&"agent"));
        assert!(types.contains(&"slash"));
        assert!(types.contains(&"mcp"));
        assert!(types.contains(&"gateway"));
    }

    #[test]
    fn test_built_in_adapters() {
        let provider = BuiltInAdapters::new();
        let adapters = provider.adapters();
        assert!(!adapters.is_empty());
        assert_eq!(adapters.len(), 7);
    }
}
