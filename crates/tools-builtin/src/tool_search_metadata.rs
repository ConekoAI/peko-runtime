//! Static metadata for the synthetic `__tool_search` stub.
//!
//! Phase 9b.N.5b.9d lifted the pure-data helpers
//! (`synthetic_description`, `synthetic_parameters`,
//! `TOOL_SEARCH_TOOL_NAME`, `TOOL_SEARCH_DEFAULT_LIMIT`) from
//! `src/tools/builtin/tool_search.rs` into `peko_tools_builtin` so
//! `peko_engine`'s agentic loop can render the catalog entry for
//! the deferred-tool search stub without depending on the root
//! crate's `ExtensionCore` (which the `ToolSearchTool` impl itself
//! uses for `Weak<ExtensionCore>` upgrade-at-execute).
//!
//! The actual `ToolSearchTool` impl stays in root for now — it
//! needs `Arc<ExtensionCore>` access for the catalog walk inside
//! `execute`, which hasn't lifted to the workspace. When the
//! `ToolFunnel` trait gains a `list_deferred_tool_definitions`
//! method (or a similar workspace-port), the impl can move
//! alongside.
//!
//! Mirrors codex `codex-rs/core/src/tools/handlers/tool_search.rs`
//! `ToolSearchHandler` static-spec shape; see that source for
//! upstream context.

use serde_json::json;

/// Default page size for `__tool_search` when `limit` is omitted.
pub const TOOL_SEARCH_DEFAULT_LIMIT: u32 = 8;

/// Sentinel tool name for the synthetic search stub.
pub const TOOL_SEARCH_TOOL_NAME: &str = "__tool_search";

/// Static description for the `__tool_search` catalog entry.
///
/// Kept separate from the per-instance `Tool::description()` so the
/// agentic loop can render the catalog entry without holding a
/// `ToolSearchTool` instance (the loop runs before the tool is
/// constructed in `Agent::init_builtins_async`).
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

/// Static JSON-Schema for the `__tool_search` tool, mirroring the
/// per-instance `Tool::parameters()`. Keep these in sync if the live
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
