//! BackgroundRuntimeAdapter trait
//!
//! Type-specific behavior for managed background runtimes. Each runtime type
//! (MCP, Gateway, etc.) implements this trait to customize initialization,
//! health checks, crash handling, and shutdown.

use async_trait::async_trait;
use super::supervisor::ManagedRuntime;

/// Action to take when a runtime crashes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashAction {
    /// Restart the runtime
    Restart,
    /// Stop the runtime and do not restart
    Stop,
    /// Notify operator but do not auto-restart
    Escalate,
}

/// Type-specific behavior for a background runtime
#[async_trait]
pub trait BackgroundRuntimeAdapter: Send + Sync + std::fmt::Debug {
    /// Called after runtime starts, before marking Running
    async fn initialize(&self, runtime: &mut ManagedRuntime) -> anyhow::Result<()>;

    /// Periodic health check
    async fn health_check(&self, runtime: &ManagedRuntime) -> bool;

    /// Runtime crashed unexpectedly
    async fn on_crash(&self, runtime: &mut ManagedRuntime) -> CrashAction;

    /// Graceful shutdown
    async fn shutdown(&self, runtime: &mut ManagedRuntime) -> anyhow::Result<()>;
}
