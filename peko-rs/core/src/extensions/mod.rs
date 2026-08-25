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
//! ├── (gateway retired — Sprint 9 Commit 3: chat-gateway adapter
//! │   framework removed; ingress is now exclusively through
//! │   per-peer standing children under the agent-session paradigm)
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

/// Sprint 9 Commit 3: the gateway extension was retired. The
/// chat-gateway adapter framework (HTTP/WebSocket bridges) never
/// shipped a concrete integration and is no longer wired into the
/// daemon. All external ingress lands in per-peer standing children
/// under the agent-session paradigm (Phase 7 of sprint 2).

/// General extension adapter — unconstrained access to all 22 hook points.
pub mod general;

/// MCP extension — Model Context Protocol server integration.
pub mod mcp;

/// Skill extension adapter — SKILL.md-based capabilities with YAML frontmatter.
pub mod skill;

/// Agent extension adapter — AGENT.md-based prompt extensions with YAML frontmatter.
pub mod agent;

// Phase 2 PR 4 (ADR-047 §2.4): the framework `slash` adapter was
// removed. Slash dispatch is handled daemon-side by
// `crate::principal::slash::SlashDispatcher`, which only resolves
// `/help` in v0. The framework `SlashAdapter` was a no-op wrapper:
// its `register_commands_with_core` returned `Vec::new()` and no
// `COMMAND.md` installer ever wired its discovered manifests into
// the dispatcher. No behavior change after removal — same `/help`
// semantics, fewer indirections.

/// Universal tool extension — external executable tools with manifest.yaml.
pub mod universal;

/// ADR-047 §5 Phase 4: workspace-resident hook scanner. Reads
/// `<workspace>/hooks/<id>/hook.toml` and registers each binding
/// against the canonical `ExtensionCore` hook registry.
pub mod workspace_hooks;

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
        // Phase 2 PR 1 (ADR-047 §2.4): `SkillAdapter` removed. Skills
        // are now files inside the principal's workspace and are
        // resolved by `WorkspaceSkillRuntime`, not registered through
        // the extension framework.
        //
        // Phase 2 PR 2 (ADR-047 §2.3): `McpAdapter` removed. MCP
        // servers are now files inside `<workspace>/mcp/<id>/` and
        // are loaded by `workspace::load_workspace_mcp_servers` at
        // principal boot. The global `McpManager` (initialised in
        // `daemon/state.rs`) is the canonical runtime for them; the
        // `McpToolProxy` / `InjectableMcpToolProxy` types wrap its
        // tools for the principal's tool bag. The four framework
        // hooks that McpAdapter wired (AgentInit, AgentShutdown,
        // PromptSystemSection, ToolExecute) are no longer needed —
        // server lifecycle is the manager's job, MCP context is
        // rendered directly by `workspace::render_mcp_prompt_context`,
        // and tool execution goes through `McpToolProxy::execute_with_context`.
        vec![
            Box::new(agent::adapter::AgentAdapter::new()),
            // Phase 2 PR 4 (ADR-047 §2.4): SlashAdapter removed. The
            // framework wrapper was a no-op (`register_commands_with_core`
            // returned `Vec::new()`); slash dispatch is handled
            // daemon-side by `principal::slash::SlashDispatcher`,
            // which only resolves `/help` in v0 (no `COMMAND.md`
            // installer). The framework adapter added no behavior
            // beyond discovering and discarding manifests.
            //
            // Phase 2 PR 3 (ADR-047 §2.4): universal tools no longer
            // register a framework adapter. Workspace-resident tools
            // are scanned by `extensions::universal::workspace`
            // and registered via `BuiltinToolAdapter::register_tool`
            // — no framework hook layer.
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

    /// General extension type (full hook access; manifest-declarable via manifest.yaml)
    pub const GENERAL: &str = "general";

    // Sprint 9 Commit 3: `GATEWAY` constant retired along with the
    // chat-gateway adapter framework. Any historical "gateway"
    // manifest bytes in users' `extensions/` directories will fail
    // `is_valid_type` and surface as install errors, which is the
    // intended forward-only behavior.

    /// Custom extension type prefix
    pub const CUSTOM_PREFIX: &str = "custom:";

    // NOTE: `builtin` is intentionally absent — built-in tools are framework-internal
    // (compiled-in native `Tool` impls), not a manifest-declarable `extension_type`.

    /// Check if a type is valid
    #[must_use]
    pub fn is_valid_type(ext_type: &str) -> bool {
        matches!(
            ext_type,
            SKILL | AGENT | SLASH | MCP | UNIVERSAL_TOOL | GENERAL
        ) || ext_type.starts_with(CUSTOM_PREFIX)
    }

    /// Get all standard extension types
    #[must_use]
    pub fn standard_types() -> Vec<&'static str> {
        vec![SKILL, AGENT, SLASH, MCP, UNIVERSAL_TOOL, GENERAL]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_type_constants() {
        // Sprint 9 Commit 3: GATEWAY constant retired.
        assert_eq!(extension_types::SKILL, "skill");
        assert_eq!(extension_types::AGENT, "agent");
        assert_eq!(extension_types::SLASH, "slash");
        assert_eq!(extension_types::MCP, "mcp");
        assert_eq!(extension_types::UNIVERSAL_TOOL, "universal-tool");
        assert_eq!(extension_types::GENERAL, "general");
    }

    #[test]
    fn test_extension_type_validation() {
        // Sprint 9 Commit 3: "gateway" is no longer a valid type.
        assert!(extension_types::is_valid_type("skill"));
        assert!(extension_types::is_valid_type("agent"));
        assert!(extension_types::is_valid_type("slash"));
        assert!(extension_types::is_valid_type("mcp"));
        assert!(extension_types::is_valid_type("custom:internal"));
        assert!(!extension_types::is_valid_type("invalid"));
        assert!(!extension_types::is_valid_type("gateway"));
    }

    #[test]
    fn test_standard_types() {
        // Sprint 9 Commit 3: gateway retired from standard types.
        let types = extension_types::standard_types();
        assert!(types.contains(&"skill"));
        assert!(types.contains(&"agent"));
        assert!(types.contains(&"slash"));
        assert!(types.contains(&"mcp"));
        assert!(!types.contains(&"gateway"));
    }

    #[test]
    fn test_built_in_adapters() {
        // Sprint 9 Commit 3: GatewayAdapter retired — adapter count
        // dropped from 7 to 6.
        // Phase 2 PR 1 (ADR-047 §2.4): SkillAdapter removed — count
        // dropped from 6 to 5.
        // Phase 2 PR 2 (ADR-047 §2.3): McpAdapter removed — count
        // dropped from 5 to 4.
        // Phase 2 PR 3 (ADR-047 §2.4): UniversalToolAdapter removed
        // — count dropped from 4 to 3.
        // Phase 2 PR 4 (ADR-047 §2.4): SlashAdapter removed — count
        // dropped from 3 to 2.
        let provider = BuiltInAdapters::new();
        let adapters = provider.adapters();
        assert!(!adapters.is_empty());
        assert_eq!(adapters.len(), 2);
    }
}
