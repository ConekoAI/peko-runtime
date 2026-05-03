//! Background Runtime Manager
//!
//! Orchestrates all long-running background runtimes in the daemon:
//! - MCP servers
//! - Gateway processes/tasks
//! - Future persistent runtimes
//!
//! This is the orchestration layer. Low-level process primitives live in
//! `src/common/process/`.

// Allow dead_code during phased implementation (Phases 2-5 will use these)
#![allow(dead_code)]

pub mod adapter;
pub mod gateway_adapter;
pub mod manager;
pub mod protocol;
pub mod router;
pub mod supervisor;

pub use adapter::{BackgroundRuntimeAdapter, CrashAction};
pub use manager::{BackgroundRuntimeManager, RuntimeSummary};
pub use protocol::{GatewayPacket, GatewayResponse, GatewayRoutingConfig};
pub use router::GatewayRouter;
pub use supervisor::{ManagedRuntime, RuntimeKind, RuntimeState};
