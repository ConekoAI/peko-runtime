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
pub mod kill;
pub mod spawn;

pub use config::{ProcessSpawnConfig, RestartPolicy, RuntimeSpawnConfig};
pub use health::{HealthCheckLoop, HealthCheckFn, wait_for_healthy};
pub use kill::{graceful_shutdown, force_kill_child, is_process_running, kill_by_pid, kill_all_by_name, wait_for_exit};
pub use spawn::{spawn_process, ResolvedCommand, resolve_command};
