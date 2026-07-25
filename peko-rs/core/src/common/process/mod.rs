//! Shared process supervision primitives
//!
//! This module provides low-level process management utilities used by both
//! the MCP system and the daemon's BackgroundRuntimeManager. It lives in
//! `src/common/` to avoid upward dependencies from `mcp`/`tools` into `daemon`.
//!
//! # Architecture
//!
//! - `spawn`: Process spawning with interpreter auto-detection
//! - `kill`: Graceful shutdown (SIGTERM → wait → SIGKILL)
//! - `health`: Health check loop abstraction
//! - `config`: Configuration types (`ProcessSpawnConfig`, `RestartPolicy`)
//!
//! These primitives are intentionally low-level. Orchestration (adapter registry,
//! daemon integration) lives in `src/daemon/background_runtime/`.

// Allow dead_code during phased implementation (Phase 2 will use these)
#![allow(dead_code)]

pub mod config;
pub mod health;
pub mod job_object;
pub mod kill;
pub mod spawn;

pub use config::{ProcessSpawnConfig, RestartPolicy, RuntimeSpawnConfig};
pub use health::wait_for_healthy;
pub use job_object::JobObject;
pub use kill::{
    force_kill_child, graceful_shutdown, is_process_running, kill_all_by_name, kill_by_pid,
    wait_for_exit,
};
pub use spawn::spawn_process;
