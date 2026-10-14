//! Shared process supervision primitives
//!
//! Low-level process utilities used by the Bash tool (`JobObject`) and the
//! daemon process service (PID liveness, kill, health wait). It lives in
//! `src/common/` to avoid upward dependencies from `tools` into `daemon`.
//!
//! - `kill`: PID liveness, kill-by-PID, wait-for-exit
//! - `health`: Health check loop abstraction
//! - `job_object`: Windows kill-on-close job objects (no-op elsewhere)

// Allow dead_code during phased implementation (Phase 2 will use these)
#![allow(dead_code)]

pub mod health;
pub mod job_object;
pub mod kill;

pub use health::wait_for_healthy;
pub use job_object::JobObject;
pub use kill::{is_process_running, kill_all_by_name, kill_by_pid, wait_for_exit};
