//! `peko-tools-builtin` — Concrete built-in tool implementations.
//!
//! Phases:
//! - 10a: filesystem subset (Read, Write, Edit, Glob, Grep)
//! - 10b: cron tools (`CronCreate`/`CronDelete`/`CronList`)
//! - 10c: async control tools (`AsyncSpawn`/`AsyncOutput`/`AsyncStatus`/
//!   `AsyncList`/`AsyncStop`)
//! - 10d: planning todos (TaskCreate/TaskGet/TaskList/TaskUpdate),
//!   session introspection (`SessionTool`), and `Skill` (with YAML
//!   frontmatter parser and dynamic context preprocessor). All three
//!   tool families speak to runtime-service port traits (TodoRuntime,
//!   SessionRuntime, SkillRuntime) that the daemon/agent side
//!   implements.
//! - 10e: messaging (`Agent` tool) + `SubagentRuntime` port. Built-ins
//!   reach the per-principal `SubagentExecutor` only through the
//!   port; the heavy executor stays in root.
//!
//! Built-in tools implement [`peko_tools_core::Tool`]; the engine wires
//! them through `ExtensionCore::execute_tool_via_hook` (the F37 funnel).
//!
//! ## Module layout
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`fs`] | Read, Write, Edit, Glob, Grep — pure filesystem tools. |
//! | [`cron`] | CronCreate/CronDelete/CronList — scheduled jobs. |
//! | [`async_control`] | AsyncSpawn/AsyncOutput/AsyncStatus/AsyncList/AsyncStop. |
//! | [`tasks`] | TaskCreate/TaskGet/TaskList/TaskUpdate + TodoRuntime port. |
//! | [`session`] | SessionTool + SessionRuntime port + SessionCache placeholder. |
//! | [`skill`] | Skill + SkillRuntime port + YAML frontmatter parser + dynamic context preprocessor. |
//! | [`messaging`] | `Agent` tool + `SubagentRuntime` port + AgentConfig/SpawnError/ExecutionConfig/SubagentRunView DTOs. |
//! | [`tool_search_metadata`] | Pure-data helpers for the `__tool_search` stub (lifted in Phase 9b.N.5b.9d). |

pub mod async_control;
pub mod cron;
pub mod fs;
pub mod messaging;
pub mod paths;
pub mod session;
pub mod skill;
pub mod tasks;
pub mod tool_search_metadata;

pub use async_control::{
    AsyncListTool, AsyncOutputTool, AsyncRuntime, AsyncSpawnTool, AsyncStatusTool, AsyncStopTool,
    SharedAsyncRuntime,
};
pub use cron::{CronCreateTool, CronDeleteTool, CronListTool, CronRuntime};
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
pub use messaging::{
    AgentTool, SessionKeyProvider, SharedSubagentRuntime, SpawnAuditEvent, SpawnRequest,
    StaticSessionKeyProvider, SubagentRuntime,
};
pub use session::{SessionCache, SessionInfo, SessionTool, SharedSessionRuntime};
pub use skill::{SharedSkillRuntime, SkillEntry, SkillFrontmatter, SkillTool};
pub use tasks::{TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool, Todo, TodoStatus};
// Phase 9b.N.5b.9d: static helpers for the `__tool_search` stub. Lifted
// out of `src/tools/builtin/tool_search.rs` so `peko-engine`'s
// agentic loop can render the catalog entry without depending on
// root-only `ExtensionCore` (which the impl uses for catalog walks
// at execute time; the impl itself stays in root).
pub use tool_search_metadata::{
    synthetic_description, synthetic_parameters, TOOL_SEARCH_DEFAULT_LIMIT, TOOL_SEARCH_TOOL_NAME,
};
