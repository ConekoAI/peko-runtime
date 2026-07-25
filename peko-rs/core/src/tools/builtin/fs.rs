//! `tools::builtin::fs` — Root shim for Phase 10a.
//!
//! Phase 10a moved `Read`, `Write`, `Edit`, `Glob`, and `Grep` into
//! `peko-tools-builtin::fs`. This file is a thin re-export so every
//! pre-Phase-10 import path (`crate::tools::builtin::fs::ReadTool`,
//! `crate::tools::builtin::fs::edit::EditTool`, etc.) keeps compiling
//! unchanged. The canonical home is now
//! `peko_tools_builtin::fs::{ReadTool, WriteTool, EditTool, GlobTool, GrepTool}`.
//!
//! The crate boundary is established for these pure filesystem tools
//! first so Phase 10b+ can move the cron/async/session/skill/task/
//! messaging subsets incrementally — each of those requires a
//! runtime-service port trait the daemon/agents side implements,
//! which is a per-tool change rather than a bulk move.

pub use peko_tools_builtin::fs::{edit, glob, grep, read, write};
pub use peko_tools_builtin::fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
