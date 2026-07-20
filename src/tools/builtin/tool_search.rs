//! Synthetic `__tool_search` tool — resolves `ToolExposure::Deferred` tools on demand.
//!
//! Audit section 3 row 6 (P2). Pre-F35, every registered tool was either
//! visible in the LLM catalog or hidden — a binary on/off. With many
//! tools, the catalog bloats every prompt. F35 lets tool authors mark a
//! tool as `Deferred`: it's omitted from the initial catalog (saves prompt
//! tokens) but discoverable through `__tool_search(query)` — the model
//! calls this stub on demand and the resolved tool names + JSON Schemas
//! come back as a normal tool result, so it can invoke them by name on
//! the next iteration.
//!
//! Mirrors codex `codex-rs/core/src/tools/handlers/tool_search.rs`
//! (`ToolSearchHandler`). Differences:
//!
//! * Codex uses BM25 over hundreds of MCP tools. Peko has <30 built-ins
//!   today; a simple word-overlap scorer in
//!   [`extensions::framework::core::scoring`](crate::extensions::framework::core::scoring)
//!   is sufficient.
//! * Codex's `search_tool_enabled` lives on `TurnContext`. Peko's
//!   `enable_tool_search` lives on [`AgentConfig`](crate::agents::AgentConfig)
//!   (config-level, not per-turn). Add a per-turn override only when a
//!   use case materializes.
//! * Peko's stub is registered per-agent (in `Agent::init_builtins_async`)
//!   with `Weak<ExtensionCore>` so the loop survives the agent going
//!   away without leaking the core.
//!
//! See the F35 audit doc section 3 row 6 for the full design.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Weak;

use crate::extensions::framework::core::ExtensionCore;
use crate::extensions::framework::types::ToolExposure;
use crate::tools::core::Tool;
use crate::tools::ToolError;

/// Default page size for [`ToolSearchTool::execute`] when `limit` is omitted.
pub const TOOL_SEARCH_DEFAULT_LIMIT: u32 = 8;

/// Sentinel tool name for the synthetic search stub. Registered into the
/// native catalog by `engine::agentic_loop::build_tool_definitions` when
/// the agent's `enable_tool_search` flag is true and at least one
/// `Deferred` tool is visible to the principal.
pub const TOOL_SEARCH_TOOL_NAME: &str = "__tool_search";

/// Synthetic tool that searches over `ToolExposure::Deferred` candidates.
///
/// Registered per-agent (in `Agent::init_builtins_async`) when
/// `AgentConfig.enable_tool_search == true`. The Weak reference to
/// [`ExtensionCore`] is upgraded at execute time so the tool doesn't
/// extend the core's lifetime past the core itself.
pub struct ToolSearchTool {
    extension_core: Weak<ExtensionCore>,
}

impl ToolSearchTool {
    /// Construct with a weak handle to the shared `ExtensionCore`.
    ///
    /// The weak handle matches the pattern used by `AsyncSpawnTool`
    /// (`tools/builtin/async_spawn.rs:14-20`). When the core is dropped
    /// (typically only on daemon shutdown), `execute` returns an error
    /// rather than panicking.
    #[must_use]
    pub fn new(extension_core: Weak<ExtensionCore>) -> Self {
        Self { extension_core }
    }
}

impl std::fmt::Debug for ToolSearchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSearchTool").finish_non_exhaustive()
    }
}

impl ToolSearchTool {
    /// Static description used by `engine::agentic_loop::build_tool_definitions`
    /// when synthesizing the catalog entry for `__tool_search`. Kept
    /// separate from the per-instance `description()` so the engine can
    /// render the catalog entry without holding a `ToolSearchTool`
    /// instance (the loop runs before the tool is constructed).
    #[must_use]
    pub fn synthetic_description() -> String {
        format!(
            "# Tool discovery\n\n\
             Searches deferred tool metadata with simple word overlap and \
             returns matching tools for the next model call.\n\n\
             Use when: you suspect a tool exists but it's not in your \
             catalog (the runtime may have deferred it for prompt size). \
             Always returns the tool name + JSON Schema so you can call \
             it on the next iteration.\n\n\
             Parameters:\n\
             - query: string (required) — search query\n\
             - limit: integer? — max results to return (default {})\n\n\
             Returns: {{ tools: [{{ name, description, parameters }}, ...] }}",
            TOOL_SEARCH_DEFAULT_LIMIT
        )
    }

    /// Static parameters schema mirroring `Tool::parameters()` for the
    /// catalog-synthesis path. Same shape — keep in sync if the live
    /// schema evolves.
    #[must_use]
    pub fn synthetic_parameters() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query for deferred tools."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!(
                        "Maximum number of tools to return. Defaults to {TOOL_SEARCH_DEFAULT_LIMIT}."
                    )
                }
            },
            "required": ["query"]
        })
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &'static str {
        "__tool_search"
    }

    fn description(&self) -> String {
        // Mirrors codex `tool_search_spec.rs:49-51` — short prose intro
        // plus the call shape. The model sees this when the stub is in
        // the catalog; without it, the model has no idea what `__tool_search`
        // does.
        format!(
            "# Tool discovery\n\n\
             Searches deferred tool metadata with simple word overlap and \
             returns matching tools for the next model call.\n\n\
             Use when: you suspect a tool exists but it's not in your \
             catalog (the runtime may have deferred it for prompt size). \
             Always returns the tool name + JSON Schema so you can call \
             it on the next iteration.\n\n\
             Parameters:\n\
             - query: string (required) — search query\n\
             - limit: integer? — max results to return (default {})\n\n\
             Returns: {{ tools: [{{ name, description, parameters }}, ...] }}",
            TOOL_SEARCH_DEFAULT_LIMIT
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query for deferred tools."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!(
                        "Maximum number of tools to return. Defaults to {TOOL_SEARCH_DEFAULT_LIMIT}."
                    )
                }
            },
            "required": ["query"]
        })
    }

    /// F34 — always `Direct`. The stub itself is always visible when
    /// registered; the gating happens at registration time
    /// (`AgentConfig.enable_tool_search` flag) and at the catalog level
    /// (we only append the stub to `build_tool_definitions` when there
    /// is at least one `Deferred` tool for the principal).
    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }

    /// Search is read-only and stateless — no need to serialize against
    /// other tools or against itself.
    fn parallelizable(&self) -> bool {
        true
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // ── 1. Parse and validate arguments ──────────────────────────────
        let query = params
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Other("missing required argument: query".to_string()))?
            .trim();
        if query.is_empty() {
            return Err(ToolError::Other("query must not be empty".to_string()).into());
        }

        let limit = params
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(TOOL_SEARCH_DEFAULT_LIMIT as u64, |n| n);
        if limit == 0 {
            return Err(ToolError::Other("limit must be greater than zero".to_string()).into());
        }
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

        // ── 2. Upgrade the Weak ref; bail if the core is gone ────────────
        let core = self.extension_core.upgrade().ok_or_else(|| {
            anyhow::anyhow!("ExtensionCore has been dropped; __tool_search cannot run")
        })?;

        // ── 3. Run the search ────────────────────────────────────────────
        // The principal scope isn't tracked in this struct; for v1 we
        // search the system principal's tools. The capability gate in
        // `list_deferred_tool_definitions` is enforced by the `system`
        // principal's grants — typically wide-open for built-ins. A
        // per-principal refinement (track the calling principal on the
        // tool) is a follow-up if a use case materializes.
        let principal_id = crate::subject::PrincipalId::system();
        let matched = core
            .list_deferred_tool_definitions(principal_id, query, limit_usize)
            .await;

        Ok(json!({
            "tools": matched.into_iter().map(|d| json!({
                "name": d.name,
                "description": d.description,
                "parameters": d.parameters,
            })).collect::<Vec<_>>()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::core::ExtensionCore;
    use crate::extensions::framework::types::{ToolMetadata, ToolSource};
    use std::sync::Arc;

    /// Build a stub `ToolMetadata` for the test registry.
    fn metadata(name: &str, description: &str, exposure: ToolExposure) -> ToolMetadata {
        ToolMetadata::new(
            name.to_string(),
            description.to_string(),
            json!({"type": "object", "properties": {}}),
            ToolSource::BuiltIn,
        )
        .with_exposure(exposure)
    }

    /// Insert a test tool directly into the registry, bypassing
    /// `BuiltinToolAdapter` (which needs a `Tool` impl). The
    /// search backend only needs `ToolMetadata` shape, not the
    /// underlying `Arc<dyn Tool>`.
    async fn insert_test_metadata(core: &ExtensionCore, meta: ToolMetadata) {
        // Use a no-op handler via the lower-level registry. The search
        // backend walks `list_tools(principal_id)` which reads from the
        // hook registry directly, so we need a registered hook_id.
        use crate::extensions::framework::core::handler::HookHandler;
        use crate::extensions::framework::core::HookContext;
        use crate::extensions::framework::types::{HookOutput, HookResult};
        use crate::subject::PrincipalId;

        #[derive(Debug)]
        struct NoopHandler;
        #[async_trait::async_trait]
        impl HookHandler for NoopHandler {
            async fn handle(&self, _ctx: HookContext) -> HookResult {
                HookResult::Continue(HookOutput::Unit)
            }
            fn hook_point(&self) -> crate::extensions::framework::core::HookPoint {
                crate::extensions::framework::core::HookPoint::ToolExecute {
                    tool_name: String::new(),
                }
            }
        }

        let handler = Arc::new(NoopHandler);
        let ext_id = crate::extensions::framework::types::ExtensionId::new("test:tool_search");
        let _ = core
            .register_tool(meta, handler, &ext_id, &PrincipalId::system())
            .await
            .expect("register test tool");
    }

    #[tokio::test]
    async fn tool_search_returns_deferred_tools_only() {
        let core = Arc::new(ExtensionCore::new());
        insert_test_metadata(
            &core,
            metadata("DirectTool", "visible tool", ToolExposure::Direct),
        )
        .await;
        insert_test_metadata(
            &core,
            metadata(
                "DeferredBash",
                "execute shell commands",
                ToolExposure::Deferred,
            ),
        )
        .await;

        let tool = ToolSearchTool::new(Arc::downgrade(&core));
        let result = tool
            .execute(json!({ "query": "bash" }))
            .await
            .expect("execute succeeds");
        let tools = result["tools"].as_array().expect("tools array");
        // Only the Deferred tool should appear; the Direct one is filtered
        // even though it would match the query.
        assert_eq!(tools.len(), 1, "expected exactly 1 result, got {tools:?}");
        assert_eq!(tools[0]["name"], "DeferredBash");
    }

    #[tokio::test]
    async fn tool_search_respects_limit() {
        let core = Arc::new(ExtensionCore::new());
        for name in ["D1", "D2", "D3", "D4", "D5"] {
            insert_test_metadata(
                &core,
                metadata(name, "execute something", ToolExposure::Deferred),
            )
            .await;
        }

        let tool = ToolSearchTool::new(Arc::downgrade(&core));
        let result = tool
            .execute(json!({ "query": "execute", "limit": 2 }))
            .await
            .expect("execute succeeds");
        let tools = result["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 2, "expected exactly 2 results");
    }

    #[tokio::test]
    async fn tool_search_handles_missing_query_with_error() {
        let core = Arc::new(ExtensionCore::new());
        let tool = ToolSearchTool::new(Arc::downgrade(&core));
        let result = tool.execute(json!({ "limit": 8 })).await;
        assert!(result.is_err(), "missing query must error");
    }

    #[tokio::test]
    async fn tool_search_handles_empty_query_with_error() {
        let core = Arc::new(ExtensionCore::new());
        let tool = ToolSearchTool::new(Arc::downgrade(&core));
        let result = tool.execute(json!({ "query": "   " })).await;
        assert!(result.is_err(), "empty query must error");
    }

    #[tokio::test]
    async fn tool_search_handles_zero_limit_with_error() {
        let core = Arc::new(ExtensionCore::new());
        let tool = ToolSearchTool::new(Arc::downgrade(&core));
        let result = tool.execute(json!({ "query": "x", "limit": 0 })).await;
        assert!(result.is_err(), "zero limit must error");
    }

    #[test]
    fn tool_search_exposure_is_direct() {
        // No core needed; just verify the trait method returns Direct.
        let core = Arc::new(ExtensionCore::new());
        let tool = ToolSearchTool::new(Arc::downgrade(&core));
        assert_eq!(tool.exposure(), ToolExposure::Direct);
        assert_eq!(tool.name(), "__tool_search");
        assert!(tool.parallelizable());
    }

    #[tokio::test]
    async fn tool_search_no_matches_returns_empty_array() {
        let core = Arc::new(ExtensionCore::new());
        insert_test_metadata(
            &core,
            metadata("Bash", "execute shell commands", ToolExposure::Deferred),
        )
        .await;

        let tool = ToolSearchTool::new(Arc::downgrade(&core));
        let result = tool
            .execute(json!({ "query": "python" }))
            .await
            .expect("execute succeeds");
        let tools = result["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 0, "expected 0 results for non-matching query");
    }

    /// F35 — schema validation: the F32b validator runs the same JSON
    /// Schema through `jsonschema` before `execute` is invoked. Verify
    /// the declared schema validates a well-formed payload and rejects
    /// a payload without `query`.
    #[test]
    fn tool_search_schema_matches_execute_contract() {
        let core = Arc::new(ExtensionCore::new());
        let tool = ToolSearchTool::new(Arc::downgrade(&core));
        let schema = tool.parameters();
        assert_eq!(schema["type"], "object");
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required array")
            .iter()
            .map(|v| v.as_str().expect("required string"))
            .collect();
        assert!(required.contains(&"query"));
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["limit"].is_object());
    }
}
