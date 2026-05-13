//! Background Runtime Manager
//!
//! Orchestrates all long-running background runtimes in the daemon:
//! - MCP servers
//! - Gateway processes/tasks
//! - Future persistent runtimes
//!
//! This is the orchestration layer. Low-level process primitives live in
//! `src/common/process/`.
//!
//! # Architecture Note
//!
//! Extension-specific runtime adapters and starters live in `src/extensions/runtime/`.
//! This module contains only the generic process supervision infrastructure:
//! - `adapter`: `BackgroundRuntimeAdapter` trait
//! - `supervisor`: `ManagedRuntime`, `RuntimeKind`, spawn/stop functions
//! - `manager`: `BackgroundRuntimeManager`
//! - `starter`: `ExtensionRuntimeStarter` trait + `StarterContext`
//! - `starter_registry`: `ExtensionRuntimeStarterRegistry`

// Allow dead_code during phased implementation (Phases 2-5 will use these)
#![allow(dead_code)]

pub mod adapter;
pub mod manager;
pub mod starter;
pub mod starter_registry;
pub mod supervisor;

pub use manager::BackgroundRuntimeManager;
pub use starter::StarterContext;
pub use starter_registry::ExtensionRuntimeStarterRegistry;
pub use supervisor::RuntimeState;
