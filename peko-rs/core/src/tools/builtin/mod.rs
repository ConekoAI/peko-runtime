//! Built-in tool implementations that still live in root.
//!
//! Phase 18 deleted the pure re-export shims for `cron`, `fs`,
//! `session`, `task_*`, `cron_*`, and `async_spawn` — those tools
//! now live in `peko_tools_builtin` and consumers import them
//! directly via `peko_tools_builtin::*`. What remains here:
//!
//! - `agent_catalog`: `AgentCatalogTool` for cross-principal agent
//!   discovery. Uses root-only `crate::principal::router::AgentPromptSummary`,
//!   so the impl cannot move into the leaf crate.
//! - `async_list` / `async_output` / `async_status` / `async_stop`:
//!   re-export shims from `peko_tools_builtin::async_control` with
//!   framework-internal test fixtures that depend on
//!   `crate::extensions::framework::async_exec::executor`. Tests stay here
//!   until the fixture migrates.
//! - `bash`: real implementation (uses `tokio::process::Command`
//!   directly with root-only permission policy hooks; future Phase
//!   10+ will port it to `peko_tools_builtin::bash` once the
//!   permission port trait lands).
//! - `messaging`: real implementation of the `Agent` tool. Depends
//!   on root-only `ExtensionCore` for subagent dispatch.
//! - `skill`: real implementation. YAML frontmatter parser +
//!   dynamic context preprocessor + `Skill` tool.
//! - `tool_search`: synthetic `__tool_search` stub for
//!   `ToolExposure::Deferred` tool discovery (F35). Depends on
//!   root-only `ExtensionCore` for catalog walks.

pub mod agent_catalog;
pub mod async_list;
pub mod async_output;
pub mod async_status;
pub mod async_stop;
pub mod bash;
pub mod messaging;
pub mod skill;
pub mod tool_search;

pub use agent_catalog::AgentCatalogTool;
pub use async_list::AsyncListTool;
pub use async_output::AsyncOutputTool;
pub use async_status::AsyncStatusTool;
pub use async_stop::AsyncStopTool;
pub use bash::BashTool;
pub use messaging::{AgentTool, DynamicSessionKeyProvider};
pub use skill::SkillTool;
pub use tool_search::{ToolSearchTool, TOOL_SEARCH_DEFAULT_LIMIT, TOOL_SEARCH_TOOL_NAME};