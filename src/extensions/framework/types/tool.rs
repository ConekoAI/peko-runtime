//! Tool-related types

use crate::extensions::framework::types::HookId;
use serde::{Deserialize, Serialize};

/// Source of a tool (for metadata tracking)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Built-in tool (part of the core codebase)
    BuiltIn,
    /// MCP tool from an MCP server
    Mcp { server: String },
    /// Universal tool from an extension
    Universal { extension_id: String },
    /// General extension tool
    General { extension_id: String },
}

impl ToolSource {
    /// Get a human-readable description of the source
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            ToolSource::BuiltIn => "built-in".to_string(),
            ToolSource::Mcp { server } => format!("MCP server: {server}"),
            ToolSource::Universal { extension_id } => {
                format!("universal extension: {extension_id}")
            }
            ToolSource::General { extension_id } => format!("extension: {extension_id}"),
        }
    }
}

/// How a tool is exposed to the LLM (F34, audit section 3 row 4).
///
/// Pre-F34 peko had a binary on/off: a tool was either visible-and-callable
/// or gated by capability. F34 adds a 4-axis model so a tool author can
/// express intent without forcing the LLM (or the prompt section) into a
/// single binary choice.
///
/// Variants:
/// - [`ToolExposure::Direct`] — visible in both the prompt "Available
///   Tools" section AND the native LLM catalog. Callable by the model.
///   This is the default for every existing tool.
/// - [`ToolExposure::DirectModelOnly`] — visible in the native LLM
///   catalog (so the model can still see name + JSON Schema and call it)
///   but suppressed from the prose "Available Tools" prompt section.
///   Useful for tools whose schema is self-documenting (the model
///   doesn't need prose) or that would waste prompt tokens if duplicated.
/// - [`ToolExposure::Deferred`] — invisible to the model until F35's
///   `__tool_search` stub resolves it. Before F35 lands, behaves like
///   [`ToolExposure::Hidden`] (no prompt, no catalog).
/// - [`ToolExposure::Hidden`] — invisible to the model in BOTH surfaces.
///   Still callable programmatically (e.g., from another tool's
///   `execute`) via the framework's internal `execute_from_hook` path,
///   but the model never sees or invokes it directly. Useful for
///   telemetry-only, audit-only, or sub-tool-of-other-tool entries.
///
/// The capability gate still applies on top of exposure — a
/// `DirectModelOnly` tool without the principal's `tool:<name>` grant
/// is still hidden from both surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolExposure {
    /// Visible in prompt section AND native catalog; callable. Default.
    #[default]
    Direct,
    /// Suppressed from prompt section; visible in native catalog; callable.
    DirectModelOnly,
    /// Hidden until `__tool_search` resolves it (F35). Until F35 ships,
    /// behaves like `Hidden`.
    Deferred,
    /// Hidden from both surfaces; only callable programmatically.
    Hidden,
}

impl ToolExposure {
    /// True if the tool should appear in the prose "Available Tools"
    /// prompt section. `Direct` only.
    #[must_use]
    pub fn visible_in_prompt_section(self) -> bool {
        matches!(self, ToolExposure::Direct)
    }

    /// True if the tool should appear in the native LLM catalog
    /// (`list_tool_definitions_with_allowlist` output).
    /// `Direct`, `DirectModelOnly`, and (eventually) `Deferred`-via-search
    /// all qualify. Pre-F35, `Deferred` returns `false` because no
    /// search stub exists yet.
    #[must_use]
    pub fn visible_in_native_catalog(self) -> bool {
        matches!(self, ToolExposure::Direct | ToolExposure::DirectModelOnly)
    }
}

/// Metadata for a registered tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    /// Tool name (unique identifier)
    pub name: String,
    /// Tool description (LLM-optimized)
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
    /// Source of the tool
    pub source: ToolSource,
    /// How this tool is exposed to the LLM (F34).
    /// Defaults to [`ToolExposure::Direct`] (visible + callable).
    #[serde(default)]
    pub exposure: ToolExposure,
    /// Reserved parameters configuration
    pub reserved_params:
        crate::extensions::framework::services::reserved_params::ReservedParamsConfig,
    /// Companion hook IDs registered alongside the primary execution hook.
    /// Populated by `ExtensionCore::register_tool()` and used during
    /// `unregister_tool()` for atomic cleanup.
    #[serde(skip)]
    pub companion_hook_ids: Option<Vec<HookId>>,
}

impl ToolMetadata {
    /// Create new tool metadata
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        source: ToolSource,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            source,
            exposure: ToolExposure::default(),
            reserved_params:
                crate::extensions::framework::services::reserved_params::ReservedParamsConfig::new(),
            companion_hook_ids: None,
        }
    }

    /// Set reserved params configuration
    #[must_use]
    pub fn with_reserved_params(
        mut self,
        config: crate::extensions::framework::services::reserved_params::ReservedParamsConfig,
    ) -> Self {
        self.reserved_params = config;
        self
    }

    /// Set the LLM exposure (F34).
    #[must_use]
    pub fn with_exposure(mut self, exposure: ToolExposure) -> Self {
        self.exposure = exposure;
        self
    }

    /// Set companion hook IDs (used internally by `ExtensionCore::register_tool`).
    #[must_use]
    pub fn with_companion_hook_ids(mut self, ids: Vec<HookId>) -> Self {
        self.companion_hook_ids = Some(ids);
        self
    }

    /// Convert to `ToolDefinition` for LLM API
    #[must_use]
    pub fn to_tool_definition(&self) -> crate::providers::ToolDefinition {
        crate::providers::ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_source() {
        assert_eq!(ToolSource::BuiltIn.description(), "built-in");
        assert_eq!(
            ToolSource::Mcp {
                server: "test".to_string()
            }
            .description(),
            "MCP server: test"
        );
    }

    #[test]
    fn test_tool_metadata() {
        let meta = ToolMetadata::new(
            "test_tool",
            "A test tool",
            serde_json::json!({"type": "object"}),
            ToolSource::BuiltIn,
        );
        assert_eq!(meta.name, "test_tool");
        assert!(meta.companion_hook_ids.is_none());
    }

    /// F34 — `ToolExposure::Direct` is the default. Every existing
    /// tool that doesn't override `exposure()` gets `Direct`, which
    /// means visible in both surfaces. Proves backward-compat.
    #[test]
    fn test_tool_exposure_default_is_direct() {
        assert_eq!(ToolExposure::default(), ToolExposure::Direct);
    }

    /// F34 — exposure variants split cleanly across the two surfaces.
    /// `DirectModelOnly` is in catalog but not prompt; `Hidden` is
    /// neither; `Deferred` is neither (until F35 adds the search
    /// stub).
    #[test]
    fn test_tool_exposure_visibility_matrix() {
        assert!(ToolExposure::Direct.visible_in_prompt_section());
        assert!(ToolExposure::Direct.visible_in_native_catalog());

        assert!(!ToolExposure::DirectModelOnly.visible_in_prompt_section());
        assert!(ToolExposure::DirectModelOnly.visible_in_native_catalog());

        assert!(!ToolExposure::Deferred.visible_in_prompt_section());
        assert!(
            !ToolExposure::Deferred.visible_in_native_catalog(),
            "Deferred behaves like Hidden until F35 wires __tool_search"
        );

        assert!(!ToolExposure::Hidden.visible_in_prompt_section());
        assert!(!ToolExposure::Hidden.visible_in_native_catalog());
    }

    /// F34 — `ToolMetadata::new` defaults `exposure` to `Direct`,
    /// matching the pre-F34 surface behavior (every tool was
    /// visible-and-callable). The `with_exposure` builder chains.
    #[test]
    fn test_tool_metadata_default_exposure_is_direct() {
        let meta = ToolMetadata::new(
            "alpha",
            "alpha desc",
            serde_json::json!({"type": "object"}),
            ToolSource::BuiltIn,
        );
        assert_eq!(meta.exposure, ToolExposure::Direct);

        let meta = meta.with_exposure(ToolExposure::DirectModelOnly);
        assert_eq!(meta.exposure, ToolExposure::DirectModelOnly);
    }
}
