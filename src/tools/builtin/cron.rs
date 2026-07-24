//! `tools::builtin::cron` — Root shim for Phase 10b.
//!
//! Phase 10b moved the cron tool surface (helpers + `CronCreateTool`,
//! `CronDeleteTool`, `CronListTool`) into `peko-tools-builtin::cron`.
//! This file is a thin re-export so every pre-Phase-10 import path
//! (`crate::tools::builtin::cron::CronCreateTool`, etc.) keeps
//! compiling unchanged. The canonical home is now
//! `peko_tools_builtin::cron::{CronCreateTool, CronDeleteTool, CronListTool}`.
//!
//! The 4 DTOs (`ScheduleKind`, `DeliveryMode`, `CronJobAction`,
//! `CronJob`) and the helpers (`build_spawn_tool_job`, `render_job_list`,
//! `calculate_next_run`, …) are also re-exported from the same place —
//! see [`crate::cron`] for the daemon-side mirror that re-exports them
//! from `peko-tools-builtin` as the single source of truth.
//!
//! The tools speak to a [`peko_tools_builtin::cron::CronRuntime`] port
//! that the daemon implements (see
//! [`peko_cron::daemon_adapter::DaemonCronAdapter`]).

pub use peko_tools_builtin::cron::{
    build_send_job, build_spawn_tool_job, calculate_next_run, create, delete,
    global_runtime as get_global_runtime, list, normalize_cron_expr, render_job_list,
    resolve_delete_after_run, resolve_label, resolve_prompt, resolve_schedule_kind,
    set_global_runtime, CronCreateTool, CronDeleteTool, CronJob, CronJobAction, CronListTool,
    CronRuntime, DeliveryMode, ScheduleKind,
};
