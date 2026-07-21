//! `tools::builtin::cron_delete` — Root shim for Phase 10b.
//!
//! Phase 10b moved `CronDeleteTool` into `peko-tools-builtin::cron::delete`.
//! This file is a thin re-export so every pre-Phase-10 import path
//! (`crate::tools::builtin::cron_delete::CronDeleteTool`) keeps
//! compiling unchanged. The canonical home is now
//! `peko_tools_builtin::cron::delete::CronDeleteTool`.

pub use peko_tools_builtin::cron::delete::{CronDeleteArgs, CronDeleteTool};
