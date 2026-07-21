//! `peko-tools-builtin` — Concrete built-in tool implementations.
//!
//! Phases:
//! - 10a: filesystem subset (Read, Write, Edit, Glob, Grep)
//! - 10b: cron tools (`CronCreate`/`CronDelete`/`CronList`)
//! - 10c: async control tools (`AsyncSpawn`/`AsyncOutput`/`AsyncStatus`/
//!   `AsyncList`/`AsyncStop`)
//!
//! Future Phase 10d/10e commits will move the session/skill/task tools
//! and the messaging (subagent) tool. Each future commit introduces
//! a runtime-service port trait the agent/daemon side implements so
//! the tools here can stay free of root-only deps (no
//! `crate::ipc::DaemonClient`, no `crate::agents::SubagentExecutor`,
//! etc. — see the Phase 10 plan).
//!
//! ## What lives here
//!
//! | Module | Phase responsibility |
//! |--------|----------------------|
//! | [`fs`] | Read, Write, Edit, Glob, Grep — pure filesystem tools. |
//! | [`cron`] | CronCreate/CronDelete/CronList — scheduled jobs. |
//! | [`async_control`] | AsyncSpawn/AsyncOutput/AsyncStatus/AsyncList/AsyncStop. |
//!
//! Built-in tools implement [`peko_tools_core::Tool`]; the engine wires
//! them through `ExtensionCore::execute_tool_via_hook` (the F37 funnel).

pub mod async_control;
pub mod cron;
pub mod fs;
pub mod paths;

pub use async_control::{
    AsyncListTool, AsyncOutputTool, AsyncRuntime, AsyncSpawnTool, AsyncStatusTool, AsyncStopTool,
    SharedAsyncRuntime,
};
pub use cron::{CronCreateTool, CronDeleteTool, CronListTool, CronRuntime};
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
