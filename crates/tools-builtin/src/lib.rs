//! `peko-tools-builtin` — Concrete built-in tool implementations.
//!
//! Phase 10a ships the filesystem subset (Read, Write, Edit, Glob, Grep).
//! Future Phase 10b+ commits will move the async control family
//! (`async_*`), cron tools, session/skill/task tools, and the messaging
//! (subagent) tool. Each future commit introduces a runtime-service
//! port trait the daemon/agents side implements so the tools here can
//! stay free of root-only deps (no `crate::ipc::DaemonClient`, no
//! `crate::agents::SubagentExecutor`, etc. — see the Phase 10 plan).
//!
//! ## What lives here
//!
//! | Module | Phase 10a responsibility |
//! |--------|--------------------------|
//! | [`fs`] | Read, Write, Edit, Glob, Grep — pure filesystem tools. |
//!
//! Built-in tools implement [`peko_tools_core::Tool`]; the engine wires
//! them through `ExtensionCore::execute_tool_via_hook` (the F37 funnel).

pub mod cron;
pub mod fs;
pub mod paths;

pub use cron::{CronCreateTool, CronDeleteTool, CronListTool, CronRuntime};
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
