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
//!
//! Future Phase 10e commit will move the messaging (subagent) tool.
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

pub mod async_control;
pub mod cron;
pub mod fs;
pub mod paths;
pub mod session;
pub mod skill;
pub mod tasks;

pub use async_control::{
    AsyncListTool, AsyncOutputTool, AsyncRuntime, AsyncSpawnTool, AsyncStatusTool, AsyncStopTool,
    SharedAsyncRuntime,
};
pub use cron::{CronCreateTool, CronDeleteTool, CronListTool, CronRuntime};
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
pub use session::{SessionCache, SessionInfo, SessionTool, SharedSessionRuntime};
pub use skill::{SharedSkillRuntime, SkillEntry, SkillFrontmatter, SkillTool};
pub use tasks::{TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool, Todo, TodoStatus};
