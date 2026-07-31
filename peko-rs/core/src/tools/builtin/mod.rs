//! Built-in tool implementations — Phase F4 partial foldback.
//!
//! As of F4 the bulk of built-in tools live in root
//! (`crate::tools::builtin::*`): filesystem (read/write/edit/glob/grep),
//! async control (spawn/output/status/list/stop), session introspection,
//! skill + YAML frontmatter + dynamic context preprocessor, messaging
//! (Agent), tasks (create/get/list/update), and a couple of root-only
//! tools (Bash, AgentCatalog, tool_search). All lifted from
//! `peko-tools-builtin` in this foldback; the sat now retains only the
//! cron port (`cron/`) + `tool_search_metadata` + `paths.rs` helpers
//! + re-exports for the engine that doesn't depend on root.
//!
//! ## What stays in `peko-tools-builtin`
//!
//! - `cron` — `CronRuntime` port trait + 3 cron tool impls (create /
//!   delete / list) + the DTOs (`ScheduleKind`, `DeliveryMode`,
//!   `CronJob`, `CronJobAction`). Stays because `peko-cron` re-exports
//!   the DTOs and `peko_core::daemon::cron_runtime` implements the
//!   port trait; lifting the trait into root would reverse the
//!   leaf-crate dep direction.
//! - `tool_search_metadata` — pure-data helpers for the
//!   `__tool_search` stub. Engine reaches these via
//!   `peko_tools_builtin::tool_search_metadata::*` so it doesn't have
//!   to depend on root for static metadata.
//! - `paths` — tilde-expansion + similar path utilities. Lifted but
//!   mirrored as a thin re-export in the sat for engine consumers.

// Lifted in Phase F4:
pub mod async_control;
pub mod bash;
pub mod fs;
pub mod messaging;
pub mod paths;
pub mod plan;
pub mod session;
pub mod skill;
pub mod tasks;

// Root-only impls that didn't have a sat counterpart:
pub mod agent_catalog;
pub mod tool_search;

// Re-exports of every tool *struct* at the canonical namespace so
// `crate::tools::builtin::X` matches what existed in the
// `peko_tools_builtin::X` path before the foldback.
pub use agent_catalog::AgentCatalogTool;
pub use async_control::{
    AsyncListTool, AsyncOutputTool, AsyncRuntime, AsyncSpawnTool, AsyncStatusTool, AsyncStopTool,
    SharedAsyncRuntime,
};
pub use bash::BashTool;
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
pub use messaging::{
    AgentTool, DynamicSessionKeyProvider, SessionKeyProvider, SharedSubagentRuntime,
    SpawnAuditEvent, SpawnRequest, StaticSessionKeyProvider, SubagentRuntime,
};
pub use plan::{
    PePlanAddStepTool, PePlanCloseTool, PePlanCreateTool, PePlanGetTool, PePlanListTool,
    PePlanMarkStepTool, PePlanRecordEvidenceTool,
};
pub use session::{SessionCache, SessionInfo, SessionTool, SharedSessionRuntime};
pub use skill::{SharedSkillRuntime, SkillEntry, SkillFrontmatter, SkillTool};
pub use tasks::{TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool, Todo, TodoStatus};
pub use tool_search::{ToolSearchTool, TOOL_SEARCH_DEFAULT_LIMIT, TOOL_SEARCH_TOOL_NAME};

// Phase F4: thin re-exports of items that stay in the sat so root
// callers can keep importing them through `crate::tools::builtin::*`
// without caring which side of the fold line they're on.
pub use paths as paths_reexport;
