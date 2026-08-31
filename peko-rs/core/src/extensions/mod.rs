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

// Sprint 9 Commit 3: the gateway extension was retired. The
// chat-gateway adapter framework (HTTP/WebSocket bridges) never
// shipped a concrete integration and is no longer wired into the
// daemon. All external ingress lands in per-peer standing children
// under the agent-session paradigm (Phase 7 of sprint 2).
// PR-C.5: `extensions::general` deleted (was 850 lines including
// `adapter.rs` 832L + `command_handler.rs` 503L — the latter
// moved to `extensions::command_handler` in PR-C.3). The general
// adapter had no remaining production callers after the two
// `register_adapter` sites were removed; with it gone, no
// `ExtensionTypeAdapter` impls remain in the daemon's process.

// PR-C.3: `CommandHookHandler` lifted out of `extensions/general/`
// (503 lines) into `extensions::command_handler` because its only
// remaining production consumer is `extensions::workspace_hooks` —
// the parser-only scanner path. The general adapter no longer needs
// it (the adapter has been thinned to a no-op shell that just
// delegates to the workspace scanner); co-locating it under
// `general::` was a historical artifact.
pub mod command_handler;

/// MCP extension — Model Context Protocol server integration.
pub mod mcp;

/// Skill extension adapter — SKILL.md-based capabilities with YAML frontmatter.
pub mod skill;

/// Agent extension adapter — AGENT.md-based prompt extensions with YAML frontmatter.
pub mod agent;

// Slash dispatch retired entirely (post-slash-removal): both the
// framework `SlashAdapter` (Phase 2 PR 4) and the daemon-side
// `SlashDispatcher` (the only consumer was `/help`, which is now
// answered by the model itself from its visible prompt catalog).
// Historical `COMMAND.md` ecosystem-standard files fail
// `extension_types::is_valid_type` and surface as install errors.

/// Universal tool extension — external executable tools with manifest.yaml.
pub mod universal;

/// ADR-047 §5 Phase 4: workspace-resident hook scanner. Reads
/// `<workspace>/hooks/<id>/hook.toml` and registers each binding
/// against the canonical `ExtensionCore` hook registry.
pub mod workspace_hooks;

// PR-C: `extensions/validation.rs` (788 lines) deleted. The
// `ExtensionValidationService` it hosted had zero production callers —
// only its own unit tests referenced it. The `peko ext validate`
// subcommand was retired in Phase 5 (ADR-047 §2.1). Doc comments in
// `universal/workspace.rs` and `mcp/workspace.rs` that pointed to
// `peko ext validate` have been reworded.

// ============================================================================
// Utilities
// ============================================================================

// ============================================================================
// Built-in Adapter Provider
// ============================================================================

// PR-C: `BuiltInAdapters` (the `Vec<Box<dyn ExtensionTypeAdapter>>`
// provider) deleted. After Phase 2 PR 1/2/3/4 stripped skill/mcp/
// slash/universal adapters, only `AgentAdapter` + `GeneralExtensionAdapter`
// remained; both are now registered directly at their single call
// sites (`agents/agent.rs:443-451` for the runtime scan,
// `daemon/state.rs:791-793` for daemon startup) instead of through a
// centralized factory. The indirection added no value once the list
// shrank to two adapters — and the agent scanner's wiring was always
// more direct anyway.
//
// PR-C.5 follow-up: both single call sites were deleted once their
// adapters were gutted. The `agent/adapter.rs` shell still hosts
// `discover_agents`; the "agents" prompt section is now emitted by
// `WorkspaceAgentsPromptHandler` (Part B — registered once in
// `principal/context.rs`, scanning `<workspace>/agents/` at invoke
// time); the `general/` directory was deleted entirely.

// PR-C: the `test_built_in_adapters` assertion
// (`adapters.len() == 2`) lived here because `BuiltInAdapters`
// depended on extension type impls. With `BuiltInAdapters` gone,
// the test was deleted alongside it. The per-adapter tests in
// `extensions/{agent,general}/adapter.rs` already cover the
// `register_*` paths that mattered.

// ============================================================================
// Extension type identifiers and validation
// ============================================================================

/// Extension type identifiers and validation.
pub mod extension_types {
    /// Skill extension type (SKILL.md)
    pub const SKILL: &str = "skill";

    /// Agent extension type (AGENT.md)
    pub const AGENT: &str = "agent";

    /// MCP server extension type
    pub const MCP: &str = "mcp";

    /// Universal tool extension type
    pub const UNIVERSAL_TOOL: &str = "universal-tool";

    /// General extension type (full hook access; manifest-declarable via manifest.yaml)
    pub const GENERAL: &str = "general";

    // Sprint 9 Commit 3: `GATEWAY` constant retired along with the
    // chat-gateway adapter framework. Any historical "gateway"
    // manifest bytes in users' `extensions/` directories will fail
    // `is_valid_type` and surface as install errors, which is the
    // intended forward-only behavior.
    //
    // Post-slash-removal: `SLASH` constant retired alongside the
    // `SlashDispatcher` runtime (and the orphan `SlashAdapter` that
    // Phase 2 PR 4 had already removed). Historical "slash"
    // manifest bytes — including `COMMAND.md` files detected at
    // Tier 1 ecosystem standard in `store.rs` — fail `is_valid_type`
    // and surface as install errors, matching the GATEWAY precedent.

    /// Custom extension type prefix
    pub const CUSTOM_PREFIX: &str = "custom:";

    // NOTE: `builtin` is intentionally absent — built-in tools are framework-internal
    // (compiled-in native `Tool` impls), not a manifest-declarable `extension_type`.

    /// Check if a type is valid
    #[must_use]
    pub fn is_valid_type(ext_type: &str) -> bool {
        matches!(
            ext_type,
            SKILL | AGENT | MCP | UNIVERSAL_TOOL | GENERAL
        ) || ext_type.starts_with(CUSTOM_PREFIX)
    }

    /// Get all standard extension types
    #[must_use]
    pub fn standard_types() -> Vec<&'static str> {
        vec![SKILL, AGENT, MCP, UNIVERSAL_TOOL, GENERAL]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_type_constants() {
        // Sprint 9 Commit 3: GATEWAY constant retired.
        // Post-slash-removal: SLASH constant retired alongside SlashDispatcher.
        assert_eq!(extension_types::SKILL, "skill");
        assert_eq!(extension_types::AGENT, "agent");
        assert_eq!(extension_types::MCP, "mcp");
        assert_eq!(extension_types::UNIVERSAL_TOOL, "universal-tool");
        assert_eq!(extension_types::GENERAL, "general");
    }

    #[test]
    fn test_extension_type_validation() {
        // Sprint 9 Commit 3: "gateway" is no longer a valid type.
        // Post-slash-removal: "slash" is no longer a valid type.
        assert!(extension_types::is_valid_type("skill"));
        assert!(extension_types::is_valid_type("agent"));
        assert!(extension_types::is_valid_type("mcp"));
        assert!(extension_types::is_valid_type("custom:internal"));
        assert!(!extension_types::is_valid_type("invalid"));
        assert!(!extension_types::is_valid_type("gateway"));
        assert!(!extension_types::is_valid_type("slash"));
    }

    #[test]
    fn test_standard_types() {
        // Sprint 9 Commit 3: gateway retired from standard types.
        // Post-slash-removal: slash retired from standard types.
        let types = extension_types::standard_types();
        assert!(types.contains(&"skill"));
        assert!(types.contains(&"agent"));
        assert!(types.contains(&"mcp"));
        assert!(!types.contains(&"gateway"));
        assert!(!types.contains(&"slash"));
    }
}
