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
pub mod gateway_starter;
pub mod manager;
pub mod mcp_starter;
pub mod protocol;
pub mod router;
pub mod starter;
pub mod starter_registry;
pub mod supervisor;

pub use adapter::{BackgroundRuntimeAdapter, CrashAction};
pub use gateway_starter::GatewayRuntimeStarter;
pub use manager::{BackgroundRuntimeManager, RuntimeSummary};
pub use mcp_starter::McpRuntimeStarter;
pub use protocol::{GatewayPacket, GatewayResponse, GatewayRoutingConfig};
pub use router::GatewayRouter;
pub use starter::{ExtensionRuntimeStarter, StarterContext};
pub use starter_registry::ExtensionRuntimeStarterRegistry;
pub use supervisor::{ManagedRuntime, RuntimeKind, RuntimeState};
