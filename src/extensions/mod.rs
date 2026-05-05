//! Extensions module — Extension Type Implementations
//!
//! This module contains **extension type implementations** (MCP, Gateway, Skill,
//! Builtin, General, Universal). The generic framework lives in `crate::extension`
//! (singular).
//!
//! # Module Boundaries
//!
//! Each extension type lives in its own directory with its adapter, runtime,
//! and protocol code. Cross-extension dependencies should go through the
//! framework (`crate::extension`), not directly between extension types.
//!
//! Extension types must NOT be added to this module's submodules without
//! also providing an `ExtensionTypeAdapter` implementation.
//!
//! # Directory Layout
//!
//! ```text
//! src/extensions/
//! ├── builtin/     # Built-in tool adapter
//! ├── gateway/     # Gateway adapter, protocol, runtime
//! ├── general/     # General extension adapter
//! ├── mcp/         # MCP adapter, protocol, runtime
//! ├── migration/   # Legacy extension migration utilities
//! ├── skill/       # Skill adapter
//! └── universal/   # Universal tool adapter and protocol
//! ```

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

/// Universal tool extension — external executable tools with manifest.json.
pub mod universal;

// ============================================================================
// Utilities
// ============================================================================

/// Legacy extension migration utilities.
pub mod migration;

/// Extension type identifiers and validation.
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
